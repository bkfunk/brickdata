# LDraw test fixtures

A minimal LDraw library used by the structural geometry tests in
`crates/blockstar-core/`. Contains just enough parts and primitives for
the reference brick/plate/Technic tests to load and bake.

## Source and license

The contents of this directory are derived from the LDraw Parts
Library, distributed by [LDraw.org](https://www.ldraw.org/) under the
[Creative Commons Attribution 4.0 license](https://creativecommons.org/licenses/by/4.0/).

Files are unmodified from upstream. The exact subset vendored here was
selected by the [ldraw.rs](https://github.com/segfault87/ldraw.rs)
project as its integration-test library and copied into this repo
verbatim.

> LEGO® parts library data provided by LDraw.org, licensed under
> CC BY 4.0.

LDraw is a trademark of the Estate of James Jessiman. LEGO® is a
registered trademark of The LEGO Group, which does not sponsor,
authorize, or endorse this project.

## Updating

These files rarely change. If you ever need to refresh them, copy
`test_ldraw_library/` from a current ldraw.rs checkout. There is no
need to ship the full ~15,000-part LDraw library here — the runtime
cache build (TODO: `parts/cache.rs`) handles the full library; this
directory is only for hermetic unit / integration tests.
