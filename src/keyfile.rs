//! Файл-ключ: CSV «Тип;ID;Реальное значение;Фейковое значение».
//! Содержит реальные персональные данные — хранится только у владельца.

use std::path::Path;

use anyhow::{Context, Result};

use crate::fake::KeyEntry;

const HEADER: [&str; 4] = ["Тип", "ID", "Реальное значение", "Фейковое значение"];

pub fn write(path: &Path, entries: &[KeyEntry]) -> Result<()> {
    let mut data = Vec::new();
    data.extend_from_slice("\u{feff}".as_bytes()); // BOM — чтобы Excel открыл UTF-8 с кириллицей
    {
        let mut w = csv::WriterBuilder::new().delimiter(b';').from_writer(&mut data);
        w.write_record(HEADER)?;
        for e in entries {
            let id = e.id.map(|i| i.to_string()).unwrap_or_default();
            w.write_record([e.kind_key.as_str(), id.as_str(), e.real.as_str(), e.fake.as_str()])?;
        }
        w.flush()?;
    }
    // Через временный файл: при сбое записи прежний ключ (с фейками других файлов) не теряется.
    let tmp = path.with_extension("csv.tmp");
    std::fs::write(&tmp, data).with_context(|| format!("не записывается ключ {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("не записывается ключ {}", path.display()))
}

/// Свободное имя для прежнего ключа: «<имя>_прежний.csv», «<имя>_прежний_2.csv»…
pub fn backup_path(path: &Path) -> std::path::PathBuf {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("ключ");
    (1..)
        .map(|n| path.with_file_name(if n == 1 { format!("{stem}_прежний.csv") } else { format!("{stem}_прежний_{n}.csv") }))
        .find(|p| !p.exists())
        .unwrap()
}

pub fn read(path: &Path) -> Result<Vec<KeyEntry>> {
    let mut r = csv::ReaderBuilder::new()
        .delimiter(b';')
        .flexible(true)
        .from_path(path)
        .with_context(|| format!("не читается ключ {}", path.display()))?;
    let mut out = Vec::new();
    for rec in r.records() {
        let rec = rec?;
        if rec.len() < 4 {
            continue;
        }
        out.push(KeyEntry {
            kind_key: rec[0].to_string(),
            id: rec[1].trim().parse().ok(),
            real: rec[2].to_string(),
            fake: rec[3].to_string(),
        });
    }
    Ok(out)
}
