fn main() {
    // `sqlx::migrate!` embeds migrations at compile time. Cargo does not track
    // newly added files in a directory by itself, so explicitly invalidate the
    // API and migration-runner binaries whenever the migration set changes.
    println!("cargo:rerun-if-changed=../../infra/migrations");
}
