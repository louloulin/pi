//! `pi packages` ecosystem — install / remove / list / version /
//! list-models / update-models.
//!
//! Mirrors the upstream `packages/coding-agent/src/package-manager-cli.ts`
//! and `docs/packages.md`: package specs are parsed in [`spec`], the
//! installed set lives in a JSON registry ([`registry`]), and
//! [`installer`] copies local packages or delegates remote sources to a
//! [`installer::PackageFetcher`]. [`commands`] is the CLI entry point
//! dispatched from `main.rs`.

pub mod commands;
pub mod installer;
pub mod registry;
pub mod spec;

pub use commands::{run, CommandError, PackageCommand};
pub use installer::{
    install, remove, resolve_root, collect_extensions, InstallError, PackageFetcher, RealFetcher,
    EXTENSIONS_DIR, PACKAGES_DIR,
};
pub use registry::{PackageEntry, Registry, RegistryError};
pub use spec::{PackageSpec, SpecError};
