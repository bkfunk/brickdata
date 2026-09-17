//! Part categorization: the Blockstar category taxonomy, a classifier
//! that places a part into it, and the library's part-filtering rules.
//!
//! The taxonomy (see `docs/design/sidebar.md`) is a two-level hierarchy:
//! a dozen top-level [`Category`]s, each holding an ordered list of leaf
//! [`Subcategory`]s. [`classify`] maps a part to a subcategory; every
//! subcategory knows its parent via [`Subcategory::category`], so the
//! (category → leaves) mapping is the single source of truth for the
//! taxonomy.
//!
//! The classifier is deliberately coarse — it gets the category and the
//! obvious leaves right and is meant to be refined over time. It is not
//! a faithful port of the old Godot keyword classifier.
//!
//! ## On-disk contract (blockstar#138)
//!
//! Both enums are stored in `catalog.sqlite` as their `u16` discriminants
//! (`category_id` / `subcategory_id` on `ldraw_part`), so every `= N`
//! below is a persisted value: never renumber, reorder, or reuse one —
//! only append. This file is vendored byte-identical in blockstar-core and
//! brickdata's catalog-builder; edit it in one and copy it to the other.
//! [`taxonomy_fingerprint`] hashes every variant with its value: the
//! builder stamps it into `meta` under [`TAXONOMY_FINGERPRINT_META_KEY`],
//! and Blockstar's reader refuses a catalog whose stamp differs from the
//! fingerprint of its own copy.

use serde::{Deserialize, Serialize};

/// A top-level grouping in the part-library sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u16)]
pub enum Category {
    Bricks = 0,
    Plates = 1,
    Tiles = 2,
    Slopes = 3,
    Technic = 4,
    Electronics = 5,
    Minifigs = 6,
    ThemeElements = 7,
    Nature = 8,
    Buildings = 9,
    Vehicles = 10,
    Other = 11,
}

impl Category {
    /// All categories, in sidebar display order.
    pub fn all() -> [Category; 12] {
        [
            Category::Bricks,
            Category::Plates,
            Category::Tiles,
            Category::Slopes,
            Category::Technic,
            Category::Electronics,
            Category::Minifigs,
            Category::ThemeElements,
            Category::Nature,
            Category::Buildings,
            Category::Vehicles,
            Category::Other,
        ]
    }

    /// Human-readable category name for the sidebar header.
    pub fn display_name(self) -> &'static str {
        match self {
            Category::Bricks => "Bricks",
            Category::Plates => "Plates",
            Category::Tiles => "Tiles",
            Category::Slopes => "Slopes",
            Category::Technic => "Technic",
            Category::Electronics => "Motors and Electronics",
            Category::Minifigs => "Minifigs and Figures",
            Category::ThemeElements => "Theme elements",
            Category::Nature => "Animals and Nature",
            Category::Buildings => "Homes and Buildings",
            Category::Vehicles => "Vehicles",
            Category::Other => "Other",
        }
    }

    /// The leaf subcategories under this category, in display order.
    pub fn subcategories(self) -> &'static [Subcategory] {
        use Subcategory::*;
        match self {
            Category::Bricks => &[Bricks, BricksModified, BricksAngled, BricksRound],
            Category::Plates => &[
                Plates,
                PlatesModified,
                PlatesAngled,
                PlatesRound,
                PlatesDishes,
                PlatesBrackets,
                PlatesBaseplates,
            ],
            Category::Tiles => &[Tiles, TilesModified, TilesAngled, TilesRound],
            Category::Slopes => &[Slopes, SlopesInverted, SlopesModified, SlopesCurved],
            Category::Technic => &[
                TechnicBricks,
                TechnicPlates,
                TechnicLiftArms,
                TechnicAxles,
                TechnicPins,
                TechnicLinksAndConnectors,
                TechnicGearsAndRacks,
                TechnicFlexible,
                TechnicPanels,
                TechnicPneumatic,
                TechnicChainsConveyorsAndElevators,
                TechnicOther,
            ],
            Category::Electronics => &[
                MechanicalMotors,
                ElectricalMotors,
                HubsAndPower,
                Sensors,
                NonLegoElectronics,
            ],
            Category::Minifigs => &[
                MinifigHeads,
                MinifigTorsosAndArms,
                MinifigLegs,
                MinifigHeadgearAndHair,
                MinifigWeapons,
                MinifigAccessoriesAndTools,
                MinifigSports,
                Dolls,
                BionicleAndHeroFactory,
                Brickheadz,
            ],
            Category::ThemeElements => &[
                EnergyEffects,
                Weapons,
                SailsFlagsAndBanners,
                BoatingAndPirateElements,
                ThemeSports,
                BoxesAndContainers,
                CurrencyAndTokens,
                PolesRodsAndAntennae,
                ThemedBaseplates,
                OtherThemeElements,
            ],
            Category::Nature => &[
                Animals,
                AnimalAccessories,
                Foliage,
                Flowers,
                TreesAndTrunks,
                Landscape,
                GemsAndMinerals,
                OtherNatureElements,
            ],
            Category::Buildings => &[
                BuildingMaterials,
                WallElements,
                Doors,
                WindowFrames,
                WindowInsertsAndShutters,
                ExteriorDecoration,
                InteriorDecoration,
            ],
            Category::Vehicles => &[
                EnginesAndThrusters,
                WingsAndFuselages,
                Cockpits,
                Fins,
                Chassis,
                Windshields,
                BoatHulls,
                TrainsGeneral,
                TrainsTracks,
                TrainsMechanics,
                HubsAndWheels,
                TiresAndTreads,
                Steering,
                SuspensionsAndBrakes,
                OtherVehicle,
            ],
            Category::Other => &[Other],
        }
    }
}

/// A leaf subcategory — a part belongs to exactly one. Each variant
/// knows its parent [`Category`] via [`Subcategory::category`].
///
/// `MinifigSports`/`ThemeSports` are disambiguated by parent — they
/// share the display name "Sports" but live under different categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u16)]
pub enum Subcategory {
    // Bricks
    Bricks = 0,
    BricksModified = 1,
    BricksAngled = 2,
    BricksRound = 3,
    // Plates
    Plates = 4,
    PlatesModified = 5,
    PlatesAngled = 6,
    PlatesRound = 7,
    PlatesDishes = 8,
    PlatesBrackets = 9,
    PlatesBaseplates = 10,
    // Tiles
    Tiles = 11,
    TilesModified = 12,
    TilesAngled = 13,
    TilesRound = 14,
    // Slopes
    Slopes = 15,
    SlopesInverted = 16,
    SlopesModified = 17,
    SlopesCurved = 18,
    // Technic
    TechnicBricks = 19,
    TechnicPlates = 20,
    TechnicLiftArms = 21,
    TechnicAxles = 22,
    TechnicPins = 23,
    TechnicLinksAndConnectors = 24,
    TechnicGearsAndRacks = 25,
    TechnicFlexible = 26,
    TechnicPanels = 27,
    TechnicPneumatic = 28,
    TechnicChainsConveyorsAndElevators = 29,
    TechnicOther = 30,
    // Electronics
    MechanicalMotors = 31,
    ElectricalMotors = 32,
    HubsAndPower = 33,
    Sensors = 34,
    NonLegoElectronics = 35,
    // Minifigs
    MinifigHeads = 36,
    MinifigTorsosAndArms = 37,
    MinifigLegs = 38,
    MinifigHeadgearAndHair = 39,
    MinifigWeapons = 40,
    MinifigAccessoriesAndTools = 41,
    MinifigSports = 42,
    Dolls = 43,
    BionicleAndHeroFactory = 44,
    Brickheadz = 45,
    // ThemeElements
    EnergyEffects = 46,
    Weapons = 47,
    SailsFlagsAndBanners = 48,
    BoatingAndPirateElements = 49,
    ThemeSports = 50,
    BoxesAndContainers = 51,
    CurrencyAndTokens = 52,
    PolesRodsAndAntennae = 53,
    ThemedBaseplates = 54,
    OtherThemeElements = 55,
    // Nature
    Animals = 56,
    AnimalAccessories = 57,
    Foliage = 58,
    Flowers = 59,
    TreesAndTrunks = 60,
    Landscape = 61,
    GemsAndMinerals = 62,
    OtherNatureElements = 63,
    // Buildings
    BuildingMaterials = 64,
    WallElements = 65,
    Doors = 66,
    WindowFrames = 67,
    WindowInsertsAndShutters = 68,
    ExteriorDecoration = 69,
    InteriorDecoration = 70,
    // Vehicles
    EnginesAndThrusters = 71,
    WingsAndFuselages = 72,
    Cockpits = 73,
    Fins = 74,
    Chassis = 75,
    Windshields = 76,
    BoatHulls = 77,
    TrainsGeneral = 78,
    TrainsTracks = 79,
    TrainsMechanics = 80,
    HubsAndWheels = 81,
    TiresAndTreads = 82,
    Steering = 83,
    SuspensionsAndBrakes = 84,
    OtherVehicle = 85,
    // Other
    Other = 86,
}

impl Subcategory {
    /// Every leaf subcategory, in declaration (= discriminant) order.
    ///
    /// An explicit list because stable Rust cannot enumerate an enum's
    /// variants. Tests check it against the per-category lists in
    /// [`Category::subcategories`] and against the pinned discriminant
    /// table, so a variant missing here fails the suite.
    pub fn all() -> [Subcategory; 87] {
        use Subcategory::*;
        [
            Bricks,
            BricksModified,
            BricksAngled,
            BricksRound,
            Plates,
            PlatesModified,
            PlatesAngled,
            PlatesRound,
            PlatesDishes,
            PlatesBrackets,
            PlatesBaseplates,
            Tiles,
            TilesModified,
            TilesAngled,
            TilesRound,
            Slopes,
            SlopesInverted,
            SlopesModified,
            SlopesCurved,
            TechnicBricks,
            TechnicPlates,
            TechnicLiftArms,
            TechnicAxles,
            TechnicPins,
            TechnicLinksAndConnectors,
            TechnicGearsAndRacks,
            TechnicFlexible,
            TechnicPanels,
            TechnicPneumatic,
            TechnicChainsConveyorsAndElevators,
            TechnicOther,
            MechanicalMotors,
            ElectricalMotors,
            HubsAndPower,
            Sensors,
            NonLegoElectronics,
            MinifigHeads,
            MinifigTorsosAndArms,
            MinifigLegs,
            MinifigHeadgearAndHair,
            MinifigWeapons,
            MinifigAccessoriesAndTools,
            MinifigSports,
            Dolls,
            BionicleAndHeroFactory,
            Brickheadz,
            EnergyEffects,
            Weapons,
            SailsFlagsAndBanners,
            BoatingAndPirateElements,
            ThemeSports,
            BoxesAndContainers,
            CurrencyAndTokens,
            PolesRodsAndAntennae,
            ThemedBaseplates,
            OtherThemeElements,
            Animals,
            AnimalAccessories,
            Foliage,
            Flowers,
            TreesAndTrunks,
            Landscape,
            GemsAndMinerals,
            OtherNatureElements,
            BuildingMaterials,
            WallElements,
            Doors,
            WindowFrames,
            WindowInsertsAndShutters,
            ExteriorDecoration,
            InteriorDecoration,
            EnginesAndThrusters,
            WingsAndFuselages,
            Cockpits,
            Fins,
            Chassis,
            Windshields,
            BoatHulls,
            TrainsGeneral,
            TrainsTracks,
            TrainsMechanics,
            HubsAndWheels,
            TiresAndTreads,
            Steering,
            SuspensionsAndBrakes,
            OtherVehicle,
            Other,
        ]
    }

    /// The parent category this subcategory belongs to.
    pub fn category(self) -> Category {
        use Subcategory::*;
        match self {
            Bricks | BricksModified | BricksAngled | BricksRound => Category::Bricks,
            Plates | PlatesModified | PlatesAngled | PlatesRound | PlatesDishes
            | PlatesBrackets | PlatesBaseplates => Category::Plates,
            Tiles | TilesModified | TilesAngled | TilesRound => Category::Tiles,
            Slopes | SlopesInverted | SlopesModified | SlopesCurved => Category::Slopes,
            TechnicBricks
            | TechnicPlates
            | TechnicLiftArms
            | TechnicAxles
            | TechnicPins
            | TechnicLinksAndConnectors
            | TechnicGearsAndRacks
            | TechnicFlexible
            | TechnicPanels
            | TechnicPneumatic
            | TechnicChainsConveyorsAndElevators
            | TechnicOther => Category::Technic,
            MechanicalMotors | ElectricalMotors | HubsAndPower | Sensors | NonLegoElectronics => {
                Category::Electronics
            }
            MinifigHeads
            | MinifigTorsosAndArms
            | MinifigLegs
            | MinifigHeadgearAndHair
            | MinifigWeapons
            | MinifigAccessoriesAndTools
            | MinifigSports
            | Dolls
            | BionicleAndHeroFactory
            | Brickheadz => Category::Minifigs,
            EnergyEffects
            | Weapons
            | SailsFlagsAndBanners
            | BoatingAndPirateElements
            | ThemeSports
            | BoxesAndContainers
            | CurrencyAndTokens
            | PolesRodsAndAntennae
            | ThemedBaseplates
            | OtherThemeElements => Category::ThemeElements,
            Animals | AnimalAccessories | Foliage | Flowers | TreesAndTrunks | Landscape
            | GemsAndMinerals | OtherNatureElements => Category::Nature,
            BuildingMaterials
            | WallElements
            | Doors
            | WindowFrames
            | WindowInsertsAndShutters
            | ExteriorDecoration
            | InteriorDecoration => Category::Buildings,
            EnginesAndThrusters | WingsAndFuselages | Cockpits | Fins | Chassis | Windshields
            | BoatHulls | TrainsGeneral | TrainsTracks | TrainsMechanics | HubsAndWheels
            | TiresAndTreads | Steering | SuspensionsAndBrakes | OtherVehicle => Category::Vehicles,
            Other => Category::Other,
        }
    }

    /// Human-readable subcategory name for the sidebar.
    pub fn display_name(self) -> &'static str {
        use Subcategory::*;
        match self {
            Bricks => "Bricks",
            BricksModified => "Bricks, Modified",
            BricksAngled => "Bricks, Angled",
            BricksRound => "Bricks, Round",
            Plates => "Plates",
            PlatesModified => "Plates, Modified",
            PlatesAngled => "Plates, Angled",
            PlatesRound => "Plates, Round",
            PlatesDishes => "Plates, Dishes",
            PlatesBrackets => "Plates, Brackets",
            PlatesBaseplates => "Plates, Baseplates",
            Tiles => "Tiles",
            TilesModified => "Tiles, Modified",
            TilesAngled => "Tiles, Angled",
            TilesRound => "Tiles, Round",
            Slopes => "Slopes",
            SlopesInverted => "Slopes, Inverted",
            SlopesModified => "Slopes, Modified",
            SlopesCurved => "Slopes, Curved",
            TechnicBricks => "Technic, Bricks",
            TechnicPlates => "Technic, Plates",
            TechnicLiftArms => "Technic, Lift arms",
            TechnicAxles => "Technic, Axles",
            TechnicPins => "Technic, Pins",
            TechnicLinksAndConnectors => "Technic, Links and Connectors",
            TechnicGearsAndRacks => "Technic, Gears and Racks",
            TechnicFlexible => "Technic, Flexible",
            TechnicPanels => "Technic, Panels",
            TechnicPneumatic => "Technic, Pneumatic",
            TechnicChainsConveyorsAndElevators => "Technic, Chains, Conveyors, and Elevators",
            TechnicOther => "Technic, Other",
            MechanicalMotors => "Mechanical Motors",
            ElectricalMotors => "Electrical Motors",
            HubsAndPower => "Hubs and Power",
            Sensors => "Sensors",
            NonLegoElectronics => "Non-Lego Electronics",
            MinifigHeads => "Minifig, Heads",
            MinifigTorsosAndArms => "Minifig, Torsos and Arms",
            MinifigLegs => "Minifig, Legs",
            MinifigHeadgearAndHair => "Minifig, Headgear and Hair",
            MinifigWeapons => "Minifig, Weapons",
            MinifigAccessoriesAndTools => "Minifig, Accessories and Tools",
            MinifigSports => "Sports",
            Dolls => "Dolls",
            BionicleAndHeroFactory => "Bionicle and Hero Factory",
            Brickheadz => "Brickheadz",
            EnergyEffects => "Energy effects",
            Weapons => "Weapons",
            SailsFlagsAndBanners => "Sails, Flags, and Banners",
            BoatingAndPirateElements => "Boating and Pirate Elements",
            ThemeSports => "Sports",
            BoxesAndContainers => "Boxes and containers",
            CurrencyAndTokens => "Currency and Tokens",
            PolesRodsAndAntennae => "Poles, Rods, and Antennae",
            ThemedBaseplates => "Themed baseplates",
            OtherThemeElements => "Other theme elements",
            Animals => "Animals",
            AnimalAccessories => "Animal accessories",
            Foliage => "Foliage",
            Flowers => "Flowers",
            TreesAndTrunks => "Trees and Trunks",
            Landscape => "Landscape",
            GemsAndMinerals => "Gems and minerals",
            OtherNatureElements => "Other nature elements",
            BuildingMaterials => "Building materials",
            WallElements => "Wall Elements",
            Doors => "Doors",
            WindowFrames => "Window Frames",
            WindowInsertsAndShutters => "Window Inserts and Shutters",
            ExteriorDecoration => "Exterior Decoration",
            InteriorDecoration => "Interior Decoration",
            EnginesAndThrusters => "Engines and Thrusters",
            WingsAndFuselages => "Wings and Fuselages",
            Cockpits => "Cockpits",
            Fins => "Fins",
            Chassis => "Chassis",
            Windshields => "Windshields",
            BoatHulls => "Boat hulls",
            TrainsGeneral => "Trains, General",
            TrainsTracks => "Trains, Tracks",
            TrainsMechanics => "Trains, Mechanics",
            HubsAndWheels => "Hubs and Wheels",
            TiresAndTreads => "Tires and Treads",
            Steering => "Steering",
            SuspensionsAndBrakes => "Suspensions and Brakes",
            OtherVehicle => "Other vehicle",
            Other => "Other",
        }
    }
}

/// `meta` key under which the catalog builder stamps [`taxonomy_fingerprint`].
pub const TAXONOMY_FINGERPRINT_META_KEY: &str = "taxonomy_fingerprint";

/// [`taxonomy_fingerprint`] of the taxonomy as declared in this file.
///
/// Changing any discriminant above — or adding or removing a variant —
/// must change this constant *deliberately*: a catalog stamped with the
/// old value stops opening in a reader built from the new one, so the
/// change has to ship with a new `catalog.sqlite` release. Update it only
/// by pasting the value the failing `taxonomy_fingerprint_is_pinned` test
/// reports, in the same commit as the taxonomy change.
pub const PINNED_TAXONOMY_FINGERPRINT: &str = "4f8338a4ae0598db";

/// Canonical text the fingerprint hashes: one `Kind:Name=value` line per
/// variant, all categories first, each list in discriminant order.
fn taxonomy_manifest() -> String {
    let mut text = String::new();
    for category in Category::all() {
        text.push_str(&format!("Category:{category:?}={}\n", category as u16));
    }
    for sub in Subcategory::all() {
        text.push_str(&format!("Subcategory:{sub:?}={}\n", sub as u16));
    }
    text
}

/// 64-bit FNV-1a of `bytes`. Dependency-free on purpose: this file is
/// compiled in two crates and must not pull anything beyond `std` (and
/// serde) into either.
fn fnv1a_64(bytes: impl IntoIterator<Item = u8>) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    bytes.into_iter().fold(OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(PRIME)
    })
}

/// Fingerprint of the taxonomy's on-disk encoding: FNV-1a over
/// [`taxonomy_manifest`], as 16 lowercase hex digits. Any change to a
/// variant's name or value changes it; see the module docs for how the
/// builder and reader use it.
pub fn taxonomy_fingerprint() -> String {
    format!("{:016x}", fnv1a_64(taxonomy_manifest().bytes()))
}

/// Theme keywords that exclude a part from the buildable library.
const EXCLUDE_KEYWORDS: [&str; 4] = ["duplo", "fabuland", "scala", "znap"];

/// Description keywords that mark a part as decorated (printed).
const DECORATED_KEYWORDS: [&str; 5] = ["sticker", "pattern", "print", "decal", "cardboard"];

/// Whether a part should be left out of the buildable catalog entirely.
///
/// `description` is the part's first-line description; `name` is its
/// `.dat` filename stem. Subparts (`parts/s/`) and primitives (`p/`,
/// including the `8/` and `48/` resolution variants) are excluded
/// structurally by the catalog builder, which only walks the top-level
/// `parts/*.dat` set — so this function doesn't re-check directory
/// prefixes.
pub fn is_excluded(description: &str, name: &str) -> bool {
    let description = description.trim();
    // Per LDraw convention, any first-line description starting with `~`
    // means non-buildable — both retired parts and rename markers like
    // "~Moved to 3010". See docs/design/importing_parts.md.
    if description.starts_with('~') {
        return true;
    }
    // Internal / helper parts are named with a leading underscore.
    if name.starts_with('_') {
        return true;
    }
    let lower = description.to_lowercase();
    EXCLUDE_KEYWORDS.iter().any(|k| lower.contains(k))
}

/// Whether a file's `0 !LDRAW_ORG` value marks it as a user-pickable design.
///
/// `ldraw_org` is the raw header value (type, optional qualifiers, and release
/// tag — e.g. `"Part"`, `"Shortcut UPDATE 2023-01"`, `"Part Alias"`). Per
/// `docs/lego-reference/ldraw-part-numbering.md` only `Part` and `Shortcut`
/// (official or `Unofficial_*`) are pickable, and the `Alias`,
/// `Flexible_Section`, and `Physical_Colour` qualifiers exclude a file even
/// when its type is otherwise pickable. This is the authoritative classifier,
/// so it filters alias / flexible-section files that live directly in `parts/`.
///
/// `None` (no `!LDRAW_ORG` line) returns `true`: the file falls back to the
/// description/name heuristics in [`is_excluded`] rather than being dropped, so
/// non-standard libraries without the meta line still catalog their parts.
pub fn is_pickable_type(ldraw_org: Option<&str>) -> bool {
    let Some(value) = ldraw_org else {
        return true;
    };
    let mut tokens = value.split_whitespace();
    let Some(kind) = tokens.next() else {
        return true;
    };
    let pickable_type = matches!(
        kind,
        "Part" | "Shortcut" | "Unofficial_Part" | "Unofficial_Shortcut"
    );
    // A non-pickable qualifier excludes even a Part/Shortcut.
    let excluded_qualifier =
        tokens.any(|t| matches!(t, "Alias" | "Flexible_Section" | "Physical_Colour"));
    pickable_type && !excluded_qualifier
}

/// Whether a part is decorated (printed, stickered, etc.).
pub fn is_decorated(description: &str) -> bool {
    let lower = description.to_lowercase();
    DECORATED_KEYWORDS.iter().any(|k| lower.contains(k))
}

/// Classify a part into a [`Subcategory`].
///
/// `ldraw_category` is the part's `!CATEGORY` meta value when present;
/// otherwise the LDraw convention is that the category is the first word
/// of the description (e.g. "Brick 2 x 4" → "Brick").
pub fn classify(description: &str, ldraw_category: Option<&str>) -> Subcategory {
    let ldraw_category = ldraw_category
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .or_else(|| description.split_whitespace().next())
        .unwrap_or("");

    let base = map_ldraw_category(ldraw_category);
    refine(base, description)
}

/// Map an LDraw category word to a leaf subcategory. Coarse by design —
/// unrecognized categories fall through to [`Subcategory::Other`].
fn map_ldraw_category(category: &str) -> Subcategory {
    use Subcategory::*;
    match category.to_lowercase().as_str() {
        "brick" => Bricks,
        "arch" | "hinge" => BricksModified,
        "cone" | "cylinder" | "sphere" => BricksRound,
        "plate" => Plates,
        "baseplate" => PlatesBaseplates,
        "bracket" => PlatesBrackets,
        "dish" => PlatesDishes,
        "tile" => Tiles,
        "slope" | "wedge" => Slopes,
        "technic" => TechnicOther,
        "electric" => HubsAndPower,
        "minifig" => MinifigAccessoriesAndTools,
        "figure" => Dolls,
        "animal" => Animals,
        "plant" => Foliage,
        "rock" => Landscape,
        "door" => Doors,
        "window" => WindowFrames,
        "glass" => WindowInsertsAndShutters,
        "fence" | "panel" => WallElements,
        "staircase" | "stairs" => BuildingMaterials,
        "bar" | "antenna" => PolesRodsAndAntennae,
        "flag" => SailsFlagsAndBanners,
        "container" => BoxesAndContainers,
        "boat" => BoatHulls,
        "car" => Chassis,
        "plane" | "wing" => WingsAndFuselages,
        "train" => TrainsGeneral,
        "wheel" => HubsAndWheels,
        "tyre" => TiresAndTreads,
        "windscreen" => Windshields,
        _ => Other,
    }
}

/// Refine a base subcategory with description keywords (e.g. "Round").
///
/// Only narrows the *default* leaf within a category — a part already
/// classified as `PlatesDishes` stays a dish even if the description
/// also says "round". This protects specific LDraw `!CATEGORY` hits
/// from being overwritten by description fallbacks.
fn refine(base: Subcategory, description: &str) -> Subcategory {
    use Subcategory::*;
    let lower = description.to_lowercase();
    match base {
        Bricks if lower.contains("round") => BricksRound,
        Plates if lower.contains("round") => PlatesRound,
        Tiles if lower.contains("round") => TilesRound,
        Slopes if lower.contains("inverted") => SlopesInverted,
        Slopes if lower.contains("curved") => SlopesCurved,
        _ => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taxonomy_covers_every_category() {
        for category in Category::all() {
            assert!(!category.display_name().is_empty());
            assert!(
                !category.subcategories().is_empty(),
                "{category:?} has no leaf subcategories",
            );
            for sub in category.subcategories() {
                assert_eq!(
                    sub.category(),
                    category,
                    "{sub:?} listed under {category:?} but reports a different parent",
                );
                assert!(!sub.display_name().is_empty());
            }
        }
    }

    #[test]
    fn classifies_basic_part_types() {
        assert_eq!(classify("Brick 2 x 4", None).category(), Category::Bricks);
        assert_eq!(classify("Plate 1 x 1", None).category(), Category::Plates);
        assert_eq!(classify("Tile 1 x 1", None).category(), Category::Tiles);
        assert_eq!(
            classify("Slope Brick 45 2 x 1", Some("Slope")).category(),
            Category::Slopes,
        );
    }

    #[test]
    fn first_word_is_used_when_no_ldraw_category() {
        // No !CATEGORY meta: the category is the description's first word.
        let sub = classify("Technic Axle 2", None);
        assert_eq!(sub.category(), Category::Technic);
    }

    #[test]
    fn refines_default_leaf_from_description_keywords() {
        assert_eq!(
            classify("Plate 1 x 1 Round", None),
            Subcategory::PlatesRound
        );
        assert_eq!(
            classify("Brick 2 x 2 Round", None),
            Subcategory::BricksRound
        );
        let inverted = classify("Slope Brick 45 2 x 1 Inverted", Some("Slope"));
        assert_eq!(inverted, Subcategory::SlopesInverted);
    }

    #[test]
    fn refine_does_not_overwrite_specific_leaves() {
        // A part already classified by !CATEGORY as a dish/bracket/
        // baseplate keeps that leaf even when its description also says
        // "round" — refine only narrows the group's default leaf.
        assert_eq!(
            classify("Dish 6 x 6 Inverted Round", Some("Dish")),
            Subcategory::PlatesDishes,
        );
        assert_eq!(
            classify("Bracket 1 x 1 Round", Some("Bracket")),
            Subcategory::PlatesBrackets,
        );
        assert_eq!(
            classify("Baseplate 16 x 16 Round", Some("Baseplate")),
            Subcategory::PlatesBaseplates,
        );
    }

    #[test]
    fn unknown_category_falls_through_to_other() {
        let sub = classify("Zorble Flange 3 x 7", None);
        assert_eq!(sub, Subcategory::Other);
        assert_eq!(sub.category(), Category::Other);
    }

    #[test]
    fn excludes_tilde_themed_and_internal_parts() {
        // Any `~`-prefixed description — rename markers AND retired
        // parts — is non-buildable per docs/design/importing_parts.md.
        assert!(is_excluded("~Moved to 3001", "3001a"));
        assert!(is_excluded("~Brick 1 x 1 (Obsolete)", "789"));
        // Theme exclusions.
        assert!(is_excluded("Duplo Brick 2 x 2", "x123"));
        assert!(is_excluded("Fabuland Door", "x456"));
        // Internal / helper parts.
        assert!(is_excluded("Brick 1 x 1 helper", "_helper"));
        // Normal parts are kept.
        assert!(!is_excluded("Brick 2 x 4", "3001"));
    }

    #[test]
    fn detects_decorated_parts() {
        assert!(is_decorated("Tile 2 x 2 with Groove with Print"));
        assert!(is_decorated("Sticker Sheet for Set 1234"));
        assert!(!is_decorated("Brick 2 x 4"));
    }

    #[test]
    fn ldraw_org_type_classifies_pickability() {
        // Pickable types (official and unofficial), with or without a tag.
        assert!(is_pickable_type(Some("Part")));
        assert!(is_pickable_type(Some("Shortcut UPDATE 2023-01")));
        assert!(is_pickable_type(Some("Unofficial_Part")));
        assert!(is_pickable_type(Some("Unofficial_Shortcut")));
        // Non-pickable types.
        assert!(!is_pickable_type(Some("Subpart")));
        assert!(!is_pickable_type(Some("Primitive")));
        assert!(!is_pickable_type(Some("48_Primitive")));
        // Excluding qualifiers veto an otherwise-pickable type.
        assert!(!is_pickable_type(Some("Part Alias")));
        assert!(!is_pickable_type(Some("Shortcut Alias")));
        assert!(!is_pickable_type(Some("Part Flexible_Section")));
        assert!(!is_pickable_type(Some("Part Physical_Colour")));
        // No header line → defer to the description/name heuristics.
        assert!(is_pickable_type(None));
    }
    // ---- On-disk contract (blockstar#138) --------------------------------
    //
    // The discriminants below are persisted in catalog.sqlite; see the
    // module docs. A failure in any of these tests means the taxonomy's
    // encoding moved, which is only ever acceptable together with a new
    // catalog release and a deliberate update of PINNED_TAXONOMY_FINGERPRINT.

    /// Every `Category` discriminant, pinned value by value.
    #[test]
    fn category_discriminants_are_pinned() {
        let expected: [(Category, u16); 12] = [
            (Category::Bricks, 0),
            (Category::Plates, 1),
            (Category::Tiles, 2),
            (Category::Slopes, 3),
            (Category::Technic, 4),
            (Category::Electronics, 5),
            (Category::Minifigs, 6),
            (Category::ThemeElements, 7),
            (Category::Nature, 8),
            (Category::Buildings, 9),
            (Category::Vehicles, 10),
            (Category::Other, 11),
        ];
        for (category, value) in expected {
            assert_eq!(category as u16, value, "{category:?} discriminant drifted");
        }
    }

    /// Every `Subcategory` discriminant, pinned value by value.
    #[test]
    fn subcategory_discriminants_are_pinned() {
        use Subcategory::*;
        let expected: [(Subcategory, u16); 87] = [
            (Bricks, 0),
            (BricksModified, 1),
            (BricksAngled, 2),
            (BricksRound, 3),
            (Plates, 4),
            (PlatesModified, 5),
            (PlatesAngled, 6),
            (PlatesRound, 7),
            (PlatesDishes, 8),
            (PlatesBrackets, 9),
            (PlatesBaseplates, 10),
            (Tiles, 11),
            (TilesModified, 12),
            (TilesAngled, 13),
            (TilesRound, 14),
            (Slopes, 15),
            (SlopesInverted, 16),
            (SlopesModified, 17),
            (SlopesCurved, 18),
            (TechnicBricks, 19),
            (TechnicPlates, 20),
            (TechnicLiftArms, 21),
            (TechnicAxles, 22),
            (TechnicPins, 23),
            (TechnicLinksAndConnectors, 24),
            (TechnicGearsAndRacks, 25),
            (TechnicFlexible, 26),
            (TechnicPanels, 27),
            (TechnicPneumatic, 28),
            (TechnicChainsConveyorsAndElevators, 29),
            (TechnicOther, 30),
            (MechanicalMotors, 31),
            (ElectricalMotors, 32),
            (HubsAndPower, 33),
            (Sensors, 34),
            (NonLegoElectronics, 35),
            (MinifigHeads, 36),
            (MinifigTorsosAndArms, 37),
            (MinifigLegs, 38),
            (MinifigHeadgearAndHair, 39),
            (MinifigWeapons, 40),
            (MinifigAccessoriesAndTools, 41),
            (MinifigSports, 42),
            (Dolls, 43),
            (BionicleAndHeroFactory, 44),
            (Brickheadz, 45),
            (EnergyEffects, 46),
            (Weapons, 47),
            (SailsFlagsAndBanners, 48),
            (BoatingAndPirateElements, 49),
            (ThemeSports, 50),
            (BoxesAndContainers, 51),
            (CurrencyAndTokens, 52),
            (PolesRodsAndAntennae, 53),
            (ThemedBaseplates, 54),
            (OtherThemeElements, 55),
            (Animals, 56),
            (AnimalAccessories, 57),
            (Foliage, 58),
            (Flowers, 59),
            (TreesAndTrunks, 60),
            (Landscape, 61),
            (GemsAndMinerals, 62),
            (OtherNatureElements, 63),
            (BuildingMaterials, 64),
            (WallElements, 65),
            (Doors, 66),
            (WindowFrames, 67),
            (WindowInsertsAndShutters, 68),
            (ExteriorDecoration, 69),
            (InteriorDecoration, 70),
            (EnginesAndThrusters, 71),
            (WingsAndFuselages, 72),
            (Cockpits, 73),
            (Fins, 74),
            (Chassis, 75),
            (Windshields, 76),
            (BoatHulls, 77),
            (TrainsGeneral, 78),
            (TrainsTracks, 79),
            (TrainsMechanics, 80),
            (HubsAndWheels, 81),
            (TiresAndTreads, 82),
            (Steering, 83),
            (SuspensionsAndBrakes, 84),
            (OtherVehicle, 85),
            (Other, 86),
        ];
        for (sub, value) in expected {
            assert_eq!(sub as u16, value, "{sub:?} discriminant drifted");
        }
    }

    /// Both `all()` lists carry discriminants exactly 0, 1, 2, … in order —
    /// which also means no duplicates and no gaps.
    #[test]
    fn discriminants_are_dense_from_zero() {
        let categories: Vec<u16> = Category::all().iter().map(|c| *c as u16).collect();
        assert_eq!(
            categories,
            (0..Category::all().len() as u16).collect::<Vec<_>>()
        );
        let subcategories: Vec<u16> = Subcategory::all().iter().map(|s| *s as u16).collect();
        assert_eq!(
            subcategories,
            (0..Subcategory::all().len() as u16).collect::<Vec<_>>()
        );
    }

    /// `Subcategory::all()` and the per-category `subcategories()` lists are
    /// two spellings of the same tree: same members, same order, each once.
    #[test]
    fn all_subcategories_matches_the_category_tree() {
        let from_tree: Vec<Subcategory> = Category::all()
            .iter()
            .flat_map(|category| category.subcategories().iter().copied())
            .collect();
        assert_eq!(from_tree, Subcategory::all().to_vec());
    }

    #[test]
    fn taxonomy_manifest_lists_every_variant_with_its_value() {
        let manifest = taxonomy_manifest();
        assert!(manifest.starts_with("Category:Bricks=0\nCategory:Plates=1\n"));
        assert!(manifest.contains("\nCategory:Other=11\nSubcategory:Bricks=0\n"));
        assert!(manifest.ends_with("Subcategory:Other=86\n"));
        assert_eq!(manifest.lines().count(), 12 + 87);
    }

    /// Reference vectors for FNV-1a 64 (empty input and "a").
    #[test]
    fn fnv1a_64_matches_reference_vectors() {
        assert_eq!(fnv1a_64(std::iter::empty()), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a_64(b"a".iter().copied()), 0xaf63_dc4c_8601_ec8c);
    }

    /// Pin of the whole encoding at once. If this fails you changed a
    /// persisted value: see `PINNED_TAXONOMY_FINGERPRINT` for what that
    /// obliges you to do before updating the constant.
    #[test]
    fn taxonomy_fingerprint_is_pinned() {
        let fingerprint = taxonomy_fingerprint();
        assert_eq!(fingerprint.len(), 16);
        assert!(
            fingerprint
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
        assert_eq!(fingerprint, PINNED_TAXONOMY_FINGERPRINT);
    }
}
