//! cclens CLI entry point. See `docs/specs/cli.md`.

fn main() -> anyhow::Result<()> {
    cclens::multi::cli::run()
}
