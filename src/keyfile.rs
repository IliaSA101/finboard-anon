//! Файл-ключ: CSV «Тип;ID;Реальное значение;Фейковое значение».
//! Содержит реальные персональные данные — хранится только у владельца.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use crate::fake::KeyEntry;

const HEADER: [&str; 4] = ["Тип", "ID", "Реальное значение", "Фейковое значение"];

/// Разделители, с которыми ключ читается: свой «;» и те, что ставят при пересохранении Excel с английскими
/// настройками, LibreOffice и Google Таблицы. Какой из них — решает только заголовок: он узнаётся ровно
/// при одном разделителе, так что угадывания по данным нет.
const DELIMITERS: [u8; 3] = [b';', b',', b'\t'];

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
    // Временный файл — тоже с настоящими значениями: при сбое он удаляется, а не остаётся рядом.
    let tmp = path.with_extension("csv.tmp");
    let done = std::fs::write(&tmp, data)
        .with_context(|| format!("не записывается ключ {}", tmp.display()))
        .and_then(|_| std::fs::rename(&tmp, path).with_context(|| format!("не записывается ключ {}", path.display())));
    if done.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    done
}

/// Свободное имя для прежнего ключа: «<имя>_прежний.csv», «<имя>_прежний_2.csv»…
pub fn backup_path(path: &Path) -> PathBuf {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("ключ");
    (1..)
        .map(|n| path.with_file_name(if n == 1 { format!("{stem}_прежний.csv") } else { format!("{stem}_прежний_{n}.csv") }))
        .find(|p| !p.exists())
        .unwrap()
}

/// Копия ключа перед его перезаписью: «<имя>_копия.csv», одна, обновляется каждым запуском.
/// Не «_прежний»: те не затираются — в них чужие ключи, отложенные при прогоне без загрузки.
pub fn copy_path(path: &Path) -> PathBuf {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("ключ");
    path.with_file_name(format!("{stem}_копия.csv"))
}

/// Кладёт рядом копию ключа, который сейчас будет перезаписан; возвращает её путь.
pub fn keep_copy(path: &Path) -> Result<PathBuf> {
    let copy = copy_path(path);
    std::fs::copy(path, &copy).with_context(|| {
        format!("не сохраняется копия ключа {} — ключ не перезаписан, файлы не записаны", copy.display())
    })?;
    Ok(copy)
}

pub fn read(path: &Path) -> Result<Vec<KeyEntry>> {
    let bytes = std::fs::read(path).with_context(|| format!("не читается ключ {}", path.display()))?;
    let body = bytes.strip_prefix("\u{feff}".as_bytes()).unwrap_or(&bytes);
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(Vec::new());
    }
    // Ключ, прочитанный не целиком, хуже отсутствующего: при записи в него же пропавшие соответствия
    // заменились бы новыми фейками навсегда. Поэтому любая непонятная строка — ошибка, а не пропуск.
    let damaged = |why: String| {
        anyhow!(
            "Ключ {} похоже повреждён или пересохранён в другом формате: {why}.\n\
             Ключ НЕ изменён, обезличивание не выполнено.\n\
             Как исправить: верните копию ключа (рядом лежит «{}», если ключ уже перезаписывался) \
             или сохраните его как CSV в кодировке UTF-8 с разделителем «;» и первой строкой \
             «Тип;ID;Реальное значение;Фейковое значение».",
            path.display(),
            copy_path(path).file_name().and_then(|n| n.to_str()).unwrap_or_default(),
        )
    };
    let text = std::str::from_utf8(body).map_err(|_| damaged("файл не в кодировке UTF-8".into()))?;
    let delim = DELIMITERS
        .into_iter()
        .find(|&d| header_matches(text, d))
        .ok_or_else(|| damaged("первая строка — не «Тип;ID;Реальное значение;Фейковое значение»".into()))?;
    let mut r = csv::ReaderBuilder::new().delimiter(delim).flexible(true).from_reader(text.as_bytes());
    let mut out = Vec::new();
    // Номер строки — как в Excel: заголовок первый, ячейка с переносом строки остаётся одной строкой.
    for (i, rec) in r.records().enumerate() {
        let line = i + 2;
        let rec = rec.map_err(|e| damaged(format!("строка {line}: CSV не разбирается ({e})")))?;
        if rec.iter().all(|f| f.trim().is_empty()) {
            continue; // пустая строка, например «;;;» после правки в Excel
        }
        if rec.len() < 4 || rec.iter().skip(4).any(|f| !f.trim().is_empty()) {
            return Err(damaged(format!("строка {line}: полей {} вместо 4", rec.len())));
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

/// Первая строка — заголовок ключа при этом разделителе (лишние пустые колонки допустимы).
fn header_matches(text: &str, delim: u8) -> bool {
    let mut r = csv::ReaderBuilder::new().delimiter(delim).flexible(true).has_headers(false).from_reader(text.as_bytes());
    match r.records().next() {
        Some(Ok(h)) => {
            h.len() >= 4
                && h.iter().zip(HEADER).all(|(f, want)| f.trim() == want)
                && h.iter().skip(4).all(|f| f.trim().is_empty())
        }
        _ => false,
    }
}
