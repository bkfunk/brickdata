//! Declarative ingest for the small Rebrickable CSV tables.
//!
//! A table is described as a CSV basename plus an ordered list of [`Field`]s.
//! [`ingest`] derives everything from that list — the `CREATE TABLE` (and any
//! indexes), the `INSERT`, the header validation, and the per-row binding — so
//! adding or changing a table is a data edit, not new control flow. The SQL
//! table name is always `rb_<table_stub>` (`part_categories` → `rb_part_categories`).
//!
//! A column carries a [`Kind`] (the one enum that drives both the SQL type and
//! the value binding) and a [`Role`] (its key/index position). Nullability is
//! fixed by the constructor (`text`/`int` vs `opt_text`/`opt_int`), not a later
//! mutator, so a column can't be made nullable after `.pk()`. The remaining
//! schema invariants (exactly one non-null PK, unique non-empty names) are
//! checked once in [`TableSpec::validate`].

use anyhow::{Context, Result};
use csv::StringRecord;
use flate2::read::GzDecoder;
use rusqlite::types::Value;
use rusqlite::{Connection, params_from_iter};
use std::collections::BTreeSet;
use std::fs::File;
use std::path::Path;

use crate::util::csv_filename;

/// A streaming CSV reader over a gzip-compressed table in the cache.
pub(crate) type Reader = csv::Reader<GzDecoder<File>>;

/// A column's SQL value type and nullability — the four real shapes. Drives
/// both the `CREATE TABLE` column type and how a CSV cell is bound.
#[derive(Clone, Copy)]
enum Kind {
    Text,
    OptText,
    Int,
    OptInt,
}

/// A column's role in the table key/indexing. An enum (not two bools) so
/// "primary key" and "indexed" are mutually exclusive by construction — a PK
/// is already the clustered key, so it is never separately indexed.
#[derive(Clone, Copy)]
enum Role {
    Pk,
    Indexed,
    Plain,
}

/// One column of a Rebrickable table. Built with the named constructors +
/// modifiers below, e.g. `Field::text("set_num").rename("set_num_rb").pk()`.
#[derive(Clone, Copy)]
pub(crate) struct Field {
    /// CSV header name (its position in the list is its column index).
    header_name: &'static str,
    /// SQL column name; defaults to `header_name` unless [`Field::rename`] is used.
    column_name: &'static str,
    kind: Kind,
    role: Role,
}

impl Field {
    fn new(header_name: &'static str, kind: Kind) -> Self {
        Field {
            header_name,
            column_name: header_name,
            kind,
            role: Role::Plain,
        }
    }

    /// A required `TEXT` column.
    pub(crate) fn text(header_name: &'static str) -> Self {
        Field::new(header_name, Kind::Text)
    }

    /// A nullable `TEXT` column (empty CSV field -> SQL NULL).
    pub(crate) fn opt_text(header_name: &'static str) -> Self {
        Field::new(header_name, Kind::OptText)
    }

    /// A required `INTEGER` column.
    pub(crate) fn int(header_name: &'static str) -> Self {
        Field::new(header_name, Kind::Int)
    }

    /// A nullable `INTEGER` column (empty CSV field -> SQL NULL).
    pub(crate) fn opt_int(header_name: &'static str) -> Self {
        Field::new(header_name, Kind::OptInt)
    }

    /// Give the SQL column a different name than the CSV header (e.g. the `_rb`
    /// suffix marking a Rebrickable-specific id).
    pub(crate) fn rename(mut self, column_name: &'static str) -> Self {
        self.column_name = column_name;
        self
    }

    /// Mark this the table's primary key. `validate` rejects a nullable PK.
    pub(crate) fn pk(mut self) -> Self {
        self.role = Role::Pk;
        self
    }

    /// Request a secondary index on this column (for FK reverse-lookups).
    pub(crate) fn indexed(mut self) -> Self {
        self.role = Role::Indexed;
        self
    }

    /// `INTEGER` or `TEXT` for the `CREATE TABLE` column.
    fn sql_type(&self) -> &'static str {
        match self.kind {
            Kind::Int | Kind::OptInt => "INTEGER",
            Kind::Text | Kind::OptText => "TEXT",
        }
    }

    fn nullable(&self) -> bool {
        matches!(self.kind, Kind::OptText | Kind::OptInt)
    }

    /// Read this column's value out of a CSV record, typed/parsed per its kind.
    /// An empty cell becomes `NULL` for nullable kinds and a hard error for
    /// required integers. The CSV header names any parse error; the table and
    /// SQL column are added by [`TableSpec::bind_row`].
    fn value(&self, record: &StringRecord, idx: usize) -> Result<Value> {
        let raw = cell(record, idx, self.header_name)?;
        Ok(match self.kind {
            Kind::Text => Value::Text(raw.to_owned()),
            Kind::OptText => opt(raw).map_or(Value::Null, |s| Value::Text(s.to_owned())),
            Kind::Int => Value::Integer(req_int(raw, self.header_name)?),
            Kind::OptInt => opt_int(raw, self.header_name)?.map_or(Value::Null, Value::Integer),
        })
    }
}

/// A table to ingest: its CSV basename plus its columns, in CSV order.
pub(crate) struct TableSpec<'a> {
    /// CSV basename (`parts.csv` → `"parts"`); the SQL table is `rb_<table_stub>`.
    pub table_stub: &'a str,
    /// The columns, in the order they appear in the CSV.
    pub fields: &'a [Field],
}

impl TableSpec<'_> {
    /// Check the schema invariants the types don't enforce: exactly one primary
    /// key, that PK not nullable, and unique non-empty CSV headers **and** SQL
    /// column names (a `rename()` typo would otherwise emit invalid
    /// `CREATE TABLE`/`INSERT` SQL with an opaque error). Returns an error (not a
    /// `debug_assert`, which a `--release` build would skip) and is run at the top
    /// of [`ingest`] and in the per-spec test.
    fn validate(&self) -> Result<()> {
        let pks: Vec<&Field> = self
            .fields
            .iter()
            .filter(|f| matches!(f.role, Role::Pk))
            .collect();
        // One or more PK columns (a composite key is allowed — e.g. the derived
        // fact tables key on `(design_id, color_id, set_id)`); none may be
        // nullable, since SQLite `WITHOUT ROWID` requires a non-null key.
        if pks.is_empty() {
            anyhow::bail!("rb_{}: no primary key column", self.table_stub);
        }
        if let Some(pk) = pks.iter().find(|f| f.nullable()) {
            anyhow::bail!(
                "rb_{}: primary key {:?} is nullable",
                self.table_stub,
                pk.column_name
            );
        }
        let mut headers = BTreeSet::new();
        let mut columns = BTreeSet::new();
        for f in self.fields {
            if f.header_name.is_empty() {
                anyhow::bail!(
                    "rb_{}: empty CSV header for column {:?}",
                    self.table_stub,
                    f.column_name
                );
            }
            if !headers.insert(f.header_name) {
                anyhow::bail!(
                    "rb_{}: duplicate CSV header {:?}",
                    self.table_stub,
                    f.header_name
                );
            }
            if f.column_name.is_empty() {
                anyhow::bail!(
                    "rb_{}: empty SQL column name for CSV header {:?}",
                    self.table_stub,
                    f.header_name
                );
            }
            if !columns.insert(f.column_name) {
                anyhow::bail!(
                    "rb_{}: duplicate SQL column {:?}",
                    self.table_stub,
                    f.column_name
                );
            }
        }
        Ok(())
    }

    /// The expected CSV header row, in order.
    fn headers(&self) -> Vec<&str> {
        self.fields.iter().map(|f| f.header_name).collect()
    }

    /// Index of the primary-key column, for labelling per-row insert errors.
    fn pk_index(&self) -> Option<usize> {
        self.fields.iter().position(|f| matches!(f.role, Role::Pk))
    }

    /// The `CREATE TABLE` (+ any secondary indexes). Column type is from the
    /// kind; the constraint is `PRIMARY KEY` for a lone pk, else `NOT NULL` for
    /// a required column, else nothing. A composite key (more than one `.pk()`
    /// field) is emitted as a trailing `PRIMARY KEY (a, b, …)` table constraint
    /// instead, with each key column marked `NOT NULL`. Always `WITHOUT ROWID` —
    /// these tables are looked up by their declared key.
    pub(crate) fn create_sql(&self) -> String {
        let pk_cols: Vec<&str> = self
            .fields
            .iter()
            .filter(|f| matches!(f.role, Role::Pk))
            .map(|f| f.column_name)
            .collect();
        let composite = pk_cols.len() > 1;
        let cols: Vec<String> = self
            .fields
            .iter()
            .map(|f| {
                let constraint = match f.role {
                    // A lone PK is declared inline; a composite key's columns
                    // are `NOT NULL` here and named in the table constraint below.
                    Role::Pk if !composite => " PRIMARY KEY",
                    Role::Pk => " NOT NULL",
                    _ if !f.nullable() => " NOT NULL",
                    _ => "",
                };
                format!("{} {}{constraint}", f.column_name, f.sql_type())
            })
            .collect();
        let mut body = cols.join(",\n    ");
        if composite {
            body.push_str(&format!(",\n    PRIMARY KEY ({})", pk_cols.join(", ")));
        }
        let mut sql = format!(
            "CREATE TABLE rb_{} (\n    {}\n) WITHOUT ROWID;",
            self.table_stub, body
        );
        for f in self
            .fields
            .iter()
            .filter(|f| matches!(f.role, Role::Indexed))
        {
            sql.push_str(&format!(
                "\nCREATE INDEX idx_rb_{table}_{col} ON rb_{table}({col});",
                table = self.table_stub,
                col = f.column_name,
            ));
        }
        sql
    }

    /// The parameterized `INSERT` (`?1`, `?2`, …), columns in order.
    fn insert_sql(&self) -> String {
        let cols: Vec<&str> = self.fields.iter().map(|f| f.column_name).collect();
        let placeholders: Vec<String> = (1..=self.fields.len()).map(|i| format!("?{i}")).collect();
        format!(
            "INSERT INTO rb_{} ({}) VALUES ({})",
            self.table_stub,
            cols.join(", "),
            placeholders.join(", ")
        )
    }

    /// Assert the CSV's header row exactly matches the declared columns (names,
    /// in order). Columns are read by fixed index, so a re-pinned snapshot that
    /// **reorders or inserts** a column would otherwise be silently mis-ingested
    /// (the same width passes the per-cell bounds check in [`cell`]); pinning the
    /// header turns that into a loud, actionable failure.
    ///
    /// Generic over the reader (only `headers()` is needed) so tests can
    /// validate against an in-memory CSV without a gzipped temp file.
    fn validate_header<R: std::io::Read>(&self, rdr: &mut csv::Reader<R>) -> Result<()> {
        validate_header(rdr, self.table_stub, &self.headers())
    }

    /// Bind one CSV record to the insert's parameters, in column order. A bad
    /// cell is reported with the table and the SQL column (plus the CSV header)
    /// so an ingest failure points straight at the offending column.
    fn bind_row(&self, record: &StringRecord) -> Result<Vec<Value>> {
        self.fields
            .iter()
            .enumerate()
            .map(|(i, f)| {
                f.value(record, i).with_context(|| {
                    format!(
                        "rb_{}: column {} (CSV {})",
                        self.table_stub, f.column_name, f.header_name
                    )
                })
            })
            .collect()
    }
}

/// Ingest one Rebrickable CSV into its `rb_*` table, end to end: validate the
/// spec, create the table (and indexes), check the pinned header, then stream
/// every row into a single transaction, reusing one [`StringRecord`]. Returns
/// the row count.
pub(crate) fn ingest(
    conn: &Connection,
    metadata_cache: &Path,
    spec: &TableSpec<'_>,
) -> Result<usize> {
    spec.validate()
        .with_context(|| format!("invalid rb_{} table spec", spec.table_stub))?;
    conn.execute_batch(&spec.create_sql())
        .with_context(|| format!("create rb_{} table", spec.table_stub))?;

    let mut rdr = open(metadata_cache, spec.table_stub)?;
    spec.validate_header(&mut rdr)?;

    let tx = conn
        .unchecked_transaction()
        .with_context(|| format!("begin rb_{} tx", spec.table_stub))?;
    let mut count = 0usize;
    {
        let mut stmt = tx.prepare(&spec.insert_sql())?;
        let pk_idx = spec.pk_index();
        let mut record = StringRecord::new();
        while rdr
            .read_record(&mut record)
            .with_context(|| format!("read {}.csv row", spec.table_stub))?
        {
            let values = spec.bind_row(&record)?;
            stmt.execute(params_from_iter(values.iter()))
                .with_context(|| {
                    // Label the failing row by its primary-key value.
                    let key = pk_idx.and_then(|i| record.get(i)).unwrap_or("row");
                    format!("insert rb_{} {key}", spec.table_stub)
                })?;
            count += 1;
        }
    }
    tx.commit()
        .with_context(|| format!("commit rb_{} tx", spec.table_stub))?;
    Ok(count)
}

/// Open the pinned `{table_stub}.csv.gz` in `metadata_cache` for streaming. The
/// file is guaranteed to exist and match the pin — `build` verifies the whole
/// snapshot before any ingest runs — so a failure here is a real I/O fault.
///
/// `pub(crate)` because the `inventory_parts` aggregation streams its CSV the
/// same way, but outside the declarative [`ingest`] path (it folds rows into an
/// in-memory map rather than inserting them 1:1).
pub(crate) fn open(metadata_cache: &Path, table_stub: &str) -> Result<Reader> {
    let path = metadata_cache.join(csv_filename(table_stub));
    let file = File::open(&path).with_context(|| format!("open {}", path.display()))?;
    Ok(csv::Reader::from_reader(GzDecoder::new(file)))
}

/// Assert a CSV's header row exactly matches `expected` (names, in order).
/// Shared by the declarative [`TableSpec`] path and the `inventory_parts`
/// aggregation, which reads by fixed index and so needs the same guard against a
/// re-pinned snapshot that reordered or inserted a column. `label` is the CSV
/// basename, used in the error. Generic over the reader so tests can validate an
/// in-memory CSV without a gzipped temp file.
pub(crate) fn validate_header<R: std::io::Read>(
    rdr: &mut csv::Reader<R>,
    label: &str,
    expected: &[&str],
) -> Result<()> {
    let header = rdr
        .headers()
        .with_context(|| format!("read {label}.csv header"))?;
    let actual: Vec<&str> = header.iter().collect();
    if actual != expected {
        anyhow::bail!(
            "{label}.csv header does not match the expected schema.\n  \
             expected: {expected:?}\n  found:    {actual:?}\n\
             The pinned snapshot's columns changed — re-author the pin in \
             blockstar-data (`just mirror-rebrickable`), update the module's \
             column list to match, and copy the new pin here.",
        );
    }
    Ok(())
}

/// Borrow the value at column `idx` (named `header_name` for diagnostics) from a
/// CSV record. Unlike `&record[idx]`, this returns a contextual error rather
/// than panicking if the record is too short — so a re-pinned snapshot whose
/// column count changed fails the build loudly instead of crashing.
pub(crate) fn cell<'a>(record: &'a StringRecord, idx: usize, header_name: &str) -> Result<&'a str> {
    record.get(idx).with_context(|| {
        format!(
            "missing column {idx} ({header_name}); the CSV has {} fields — the \
             pinned schema may have changed",
            record.len()
        )
    })
}

/// Rebrickable encodes NULL as an empty field — the empty string, whether the
/// CSV writes it `,,` or `,"",` (the reader yields `""` for both); it never uses
/// `\N`. So empty -> None.
fn opt(value: &str) -> Option<&str> {
    if value.is_empty() { None } else { Some(value) }
}

/// Parse an *optional* integer column, treating an empty value as NULL. Fails
/// loudly (naming the CSV header) on a non-empty value that isn't an integer,
/// rather than silently dropping it.
fn opt_int(value: &str, header_name: &str) -> Result<Option<i64>> {
    opt(value)
        .map(str::parse::<i64>)
        .transpose()
        .with_context(|| format!("parse integer {header_name} from {value:?}"))
}

/// Parse a *required* integer column. An empty or non-numeric value is a hard
/// error here — clearer than letting an empty field reach a NOT NULL column and
/// surface as a constraint failure at insert time.
pub(crate) fn req_int(value: &str, header_name: &str) -> Result<i64> {
    if value.is_empty() {
        anyhow::bail!("required integer {header_name} is empty");
    }
    value
        .parse::<i64>()
        .with_context(|| format!("parse required integer {header_name} from {value:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_errors_with_context_on_short_record() {
        let rec = StringRecord::from(vec!["a", "b"]);
        assert_eq!(cell(&rec, 1, "name").unwrap(), "b");
        // A column past the end yields a contextual error, not a panic.
        let err = cell(&rec, 5, "design_id").expect_err("out-of-range must error");
        let msg = format!("{err:#}");
        assert!(msg.contains("design_id"), "names the header: {msg}");
        assert!(msg.contains("2 fields"), "reports actual width: {msg}");
    }

    #[test]
    fn opt_and_opt_int_map_empty_to_none() {
        assert_eq!(opt(""), None);
        assert_eq!(opt("x"), Some("x"));
        assert_eq!(opt_int("", "year").unwrap(), None);
        assert_eq!(opt_int("42", "year").unwrap(), Some(42));
        let err = opt_int("nope", "year").expect_err("non-int must error");
        assert!(
            format!("{err:#}").contains("year"),
            "names the header: {err:#}"
        );
    }

    #[test]
    fn req_int_rejects_empty_and_nonnumeric() {
        assert_eq!(req_int("7", "id").unwrap(), 7);
        let empty = req_int("", "id").expect_err("empty required int must error");
        assert!(format!("{empty:#}").contains("id is empty"));
        assert!(req_int("x", "version").is_err());
    }

    /// A spec exercising every Field flavor: text/int, required/optional, a
    /// renamed pk, and an indexed FK column.
    fn sample_spec() -> TableSpec<'static> {
        static FIELDS: [Field; 4] = [
            Field {
                header_name: "id",
                column_name: "widget_id_rb",
                kind: Kind::Int,
                role: Role::Pk,
            },
            Field {
                header_name: "name",
                column_name: "name",
                kind: Kind::Text,
                role: Role::Plain,
            },
            Field {
                header_name: "cat_id",
                column_name: "category_id_rb",
                kind: Kind::OptInt,
                role: Role::Indexed,
            },
            Field {
                header_name: "note",
                column_name: "note",
                kind: Kind::OptText,
                role: Role::Plain,
            },
        ];
        TableSpec {
            table_stub: "widgets",
            fields: &FIELDS,
        }
    }

    #[test]
    fn create_sql_derives_types_constraints_and_index() {
        let sql = sample_spec().create_sql();
        assert!(sql.contains("CREATE TABLE rb_widgets ("), "{sql}");
        assert!(sql.contains("widget_id_rb INTEGER PRIMARY KEY"), "{sql}");
        assert!(sql.contains("name TEXT NOT NULL"), "{sql}");
        assert!(
            sql.contains("category_id_rb INTEGER,"),
            "optional -> nullable: {sql}"
        );
        assert!(
            sql.contains("note TEXT\n"),
            "optional text is nullable: {sql}"
        );
        assert!(sql.contains("WITHOUT ROWID;"), "{sql}");
        assert!(
            sql.contains(
                "CREATE INDEX idx_rb_widgets_category_id_rb ON rb_widgets(category_id_rb);"
            ),
            "indexed FK gets a secondary index: {sql}"
        );
        assert_eq!(sql.matches("CREATE INDEX").count(), 1, "{sql}");
    }

    #[test]
    fn create_sql_emits_composite_primary_key() {
        let spec = TableSpec {
            table_stub: "part_color_set",
            fields: &[
                Field::text("design_id").pk(),
                Field::int("color_id").pk(),
                Field::int("set_id").pk(),
                Field::int("qty"),
                Field::opt_int("year"),
            ],
        };
        spec.validate().expect("a composite key is valid");
        let sql = spec.create_sql();
        // Key columns are NOT NULL and named in one trailing table constraint —
        // never an inline `PRIMARY KEY` on any single column.
        assert!(sql.contains("design_id TEXT NOT NULL"), "{sql}");
        assert!(sql.contains("color_id INTEGER NOT NULL"), "{sql}");
        assert!(
            sql.contains("PRIMARY KEY (design_id, color_id, set_id)"),
            "{sql}"
        );
        assert!(!sql.contains("PRIMARY KEY,"), "no inline pk: {sql}");
        // The nullable column carries no constraint (it precedes the table
        // PRIMARY KEY, so its line ends with a comma, not NOT NULL).
        assert!(
            sql.contains("year INTEGER,\n"),
            "nullable stays nullable: {sql}"
        );
        assert!(!sql.contains("year INTEGER NOT NULL"), "{sql}");
    }

    #[test]
    fn insert_sql_lists_columns_and_placeholders_in_order() {
        assert_eq!(
            sample_spec().insert_sql(),
            "INSERT INTO rb_widgets (widget_id_rb, name, category_id_rb, note) \
             VALUES (?1, ?2, ?3, ?4)"
        );
    }

    #[test]
    fn bind_row_types_and_nulls_each_cell() {
        let rec = StringRecord::from(vec!["7", "Brick", "", "hi"]);
        let values = sample_spec().bind_row(&rec).unwrap();
        assert!(matches!(values[0], Value::Integer(7)));
        assert!(matches!(&values[1], Value::Text(s) if s == "Brick"));
        assert!(
            matches!(values[2], Value::Null),
            "empty optional int -> NULL"
        );
        assert!(matches!(&values[3], Value::Text(s) if s == "hi"));
    }

    #[test]
    fn bind_row_error_names_table_and_column() {
        // A non-integer in the optional-int column.
        let rec = StringRecord::from(vec!["7", "Brick", "not-an-int", "hi"]);
        let err = sample_spec()
            .bind_row(&rec)
            .expect_err("bad int must error");
        let msg = format!("{err:#}");
        assert!(msg.contains("rb_widgets"), "names the table: {msg}");
        assert!(
            msg.contains("category_id_rb"),
            "names the SQL column: {msg}"
        );
        assert!(msg.contains("cat_id"), "names the CSV header: {msg}");
    }

    #[test]
    fn validate_rejects_bad_specs_and_accepts_good_ones() {
        sample_spec().validate().expect("the sample spec is valid");

        // A composite key (two `.pk()` fields) is valid — see the dedicated
        // composite test below; it is deliberately absent from the reject list.
        let cases: &[(&str, &[Field])] = &[
            ("nullable", &[Field::opt_int("id").rename("x").pk()]),
            ("no primary key", &[Field::text("a")]),
            // Two CSV headers collide.
            (
                "duplicate CSV header",
                &[Field::int("id").pk(), Field::text("id").rename("name")],
            ),
            // Two renames collide on the same SQL column.
            (
                "duplicate SQL column",
                &[Field::int("id").pk(), Field::text("name").rename("id")],
            ),
        ];
        for (label, fields) in cases {
            let spec = TableSpec {
                table_stub: "t",
                fields,
            };
            assert!(spec.validate().is_err(), "{label} must be rejected");
        }
    }

    #[test]
    fn validate_header_accepts_matching_and_rejects_reordered() {
        let spec = TableSpec {
            table_stub: "t",
            fields: &[Field::int("a").pk(), Field::text("b"), Field::text("c")],
        };
        // `validate_header` is generic over `Read`, so the tests feed it an
        // in-memory CSV directly — no gzipped temp file needed.
        let reader = |csv: &'static str| csv::Reader::from_reader(csv.as_bytes());

        let mut ok = reader("a,b,c\n1,2,3\n");
        spec.validate_header(&mut ok)
            .expect("matching header passes");

        // Same columns, reordered — the bug a count check misses.
        let mut reordered = reader("a,c,b\n1,3,2\n");
        let err = spec
            .validate_header(&mut reordered)
            .expect_err("reordered header must fail");
        assert!(format!("{err:#}").contains("does not match"), "{err:#}");

        // Inserted column (same prefix, wider).
        let mut widened = reader("a,b,x,c\n1,2,9,3\n");
        assert!(spec.validate_header(&mut widened).is_err());
    }
}
