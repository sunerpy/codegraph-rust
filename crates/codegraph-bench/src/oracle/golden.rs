use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use super::canonicalize::{canonicalize_db, CanonicalDb, CanonicalRow};

pub fn write_golden(db_path: &Path, outdir: &Path) -> Result<()> {
    let canonical = canonicalize_db(db_path)?;
    fs::create_dir_all(outdir).with_context(|| format!("creating {}", outdir.display()))?;
    write_json(&outdir.join("nodes.json"), &canonical.nodes)?;
    write_json(&outdir.join("edges.json"), &canonical.edges)?;
    write_json(&outdir.join("refs.json"), &canonical.unresolved_refs)?;
    write_json(&outdir.join("files.json"), &canonical.files)?;
    fs::write(outdir.join("schema.sql"), canonical.schema)
        .with_context(|| format!("writing {}/schema.sql", outdir.display()))?;
    Ok(())
}

pub fn load_golden(golden_dir: &Path) -> Result<CanonicalDb> {
    Ok(CanonicalDb {
        nodes: read_json(&golden_dir.join("nodes.json"))?,
        edges: read_json(&golden_dir.join("edges.json"))?,
        unresolved_refs: read_json(&golden_dir.join("refs.json"))?,
        files: read_json(&golden_dir.join("files.json"))?,
        schema: fs::read_to_string(golden_dir.join("schema.sql"))
            .with_context(|| format!("reading {}/schema.sql", golden_dir.display()))?,
    })
}

fn write_json(path: &Path, rows: &[CanonicalRow]) -> Result<()> {
    let json = serde_json::to_string_pretty(rows).context("serializing canonical JSON")?;
    fs::write(path, format!("{json}\n")).with_context(|| format!("writing {}", path.display()))
}

fn read_json(path: &Path) -> Result<Vec<CanonicalRow>> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}
