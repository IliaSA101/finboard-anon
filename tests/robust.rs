//! Повреждённые файлы: обработка должна завершиться ошибкой (а не зависнуть) и не оставить
//! недописанный результат рядом с выходным файлом.

mod common;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;
use std::time::Duration;

use common::*;
use finboard_anon::anon::{run, Job};
use finboard_anon::profile::{Kind, Profile, Rule, Settings};

fn book() -> Book {
    let mut b = Book { author: "Иванов Иван".into(), ..Default::default() };
    let sh = b.sheet("Сотрудники");
    sh.push((1, 0, s("ФИО")));
    for (i, n) in NAMES.iter().enumerate() {
        sh.push((2 + i as u32, 0, s(n)));
        sh.push((2 + i as u32, 1, Cell::N(100.0 + i as f64)));
    }
    b
}

/// Копия книги, в которой часть `part` обрезана сразу после первого вхождения `after`.
fn cut_part(src: &Path, dst: &Path, part: &str, after: &str) {
    let mut z = zip::ZipArchive::new(std::fs::File::open(src).unwrap()).unwrap();
    let mut w = zip::ZipWriter::new(std::fs::File::create(dst).unwrap());
    let o = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut found = false;
    for i in 0..z.len() {
        let mut f = z.by_index(i).unwrap();
        let name = f.name().to_string();
        let mut data = String::new();
        f.read_to_string(&mut data).unwrap();
        if name == part {
            let at = data.find(after).unwrap_or_else(|| panic!("в {part} нет {after}")) + after.len();
            data.truncate(at);
            found = true;
        }
        w.start_file(name, o).unwrap();
        w.write_all(data.as_bytes()).unwrap();
    }
    w.finish().unwrap();
    assert!(found, "в книге нет части {part}");
}

/// Обработка в отдельном потоке: зависание — провал теста через 20 секунд, а не вечный прогон.
fn run_with_timeout(input: PathBuf, output: PathBuf) -> Result<(), String> {
    let job = Job {
        input,
        output,
        key_in: None,
        key_out: None,
        profile: Profile {
            rules: vec![Rule::new("Сотрудники", 0, 2, Some(5), Kind::FullName)],
            settings: Settings { salt: "robust".into(), ..Settings::default() },
        },
    };
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let r = run(&job, &mut |_| {}).map(|_| ()).map_err(|e| format!("{e:#}"));
        let _ = tx.send(r);
    });
    rx.recv_timeout(Duration::from_secs(20)).expect("обработка зависла на повреждённом файле")
}

fn leftovers(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect()
}

/// Аудитор: лист, обрезанный внутри ячейки, вешал программу навсегда и оставлял x_anon.tmp.
#[test]
fn sheet_cut_inside_cell_fails_without_hang_and_tmp() {
    let dir = tmp("robust_sheet_cut");
    let good = dir.join("исходный.xlsx");
    write_xlsx(&good, &book());
    let bad = dir.join("обрезанный.xlsx");
    cut_part(&good, &bad, "xl/worksheets/sheet1.xml", r#"<c r="B3"><v>"#);
    let out = dir.join("обрезанный_anon.xlsx");
    let err = run_with_timeout(bad, out.clone()).expect_err("обрезанный лист обработан без ошибки");
    assert!(err.contains("Сотрудники"), "ошибка не называет лист: {err}");
    assert!(err.contains("B3"), "ошибка не называет ячейку: {err}");
    assert!(leftovers(&dir).is_empty(), "остался временный файл: {:?}", leftovers(&dir));
    assert!(!out.exists(), "результат записан, хотя обработка не удалась");
}

/// Та же беда в общих строках: запись <si> без конца.
#[test]
fn shared_strings_cut_inside_entry_fails_without_hang_and_tmp() {
    let dir = tmp("robust_sst_cut");
    let good = dir.join("исходный.xlsx");
    write_xlsx(&good, &book());
    let bad = dir.join("обрезанный.xlsx");
    cut_part(&good, &bad, "xl/sharedStrings.xml", "<si><t xml:space=\"preserve\">Петрова");
    let out = dir.join("обрезанный_anon.xlsx");
    let err = run_with_timeout(bad, out.clone()).expect_err("обрезанные общие строки обработаны без ошибки");
    assert!(err.contains("общие строки"), "ошибка не называет часть: {err}");
    assert!(leftovers(&dir).is_empty(), "остался временный файл: {:?}", leftovers(&dir));
    assert!(!out.exists(), "результат записан, хотя обработка не удалась");
}
