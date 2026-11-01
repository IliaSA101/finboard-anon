//! Доработки 0.4.0 по отчёту аудитора: ключ, пересохранённый в Excel/LibreOffice/Google Таблицах,
//! загружался как «0 записей» и при записи в тот же файл затирался новыми фейками — соответствия
//! уже переданных файлов терялись навсегда. Каждый тест падал до исправления.

mod common;

use std::path::{Path, PathBuf};

use common::*;
use finboard_anon::anon::{run, Job, Report};
use finboard_anon::profile::{Kind, Profile, Rule, Settings};

const KEY: &str = "КЛЮЧ_ОБЕЗЛИЧИВАНИЯ_НЕ_ПЕРЕДАВАТЬ.csv";

fn rules() -> Vec<Rule> {
    vec![Rule::new("Л", 0, 2, None, Kind::FullName), Rule::new("Л", 1, 2, None, Kind::Address)]
}

/// Книга: ФИО и адреса — в адресах запятые, чтобы ключ с разделителем «,» был в кавычках.
fn book(d: &Path) -> PathBuf {
    let input = d.join("in.xlsx");
    let mut b = Book::default();
    b.sheet("Л").extend([
        (1, 0, s("ФИО")),
        (1, 1, s("Адрес")),
        (2, 0, s("Иванов Иван Иванович")),
        (2, 1, s("г. Москва, ул. Ленина, д. 1")),
        (3, 0, s("Петрова Мария Сергеевна")),
        (3, 1, s("г. Казань, ул. Баумана, д. 5")),
    ]);
    write_xlsx(&input, &b);
    input
}

fn go(input: &Path, output: &Path, key_in: Option<&Path>, key_out: &Path, salt: &str) -> anyhow::Result<(Report, Vec<String>)> {
    let profile = Profile { rules: rules(), settings: Settings { salt: salt.into(), ..Settings::default() } };
    let job = Job { input: input.into(), output: output.into(), key_in: key_in.map(Into::into), key_out: Some(key_out.into()), profile };
    let mut log = Vec::new();
    let rep = run(&job, &mut |m| log.push(m))?;
    Ok((rep, log))
}

fn fakes(output: &Path) -> Vec<String> {
    let g = read_book(output);
    ["A2", "B2", "A3", "B3"].iter().map(|a| text(&g, "Л", a)).collect()
}

/// Ключ, как его пересохраняют программы с другим разделителем: те же поля, BOM сохраняется.
fn resave(key: &[u8], delim: u8) -> Vec<u8> {
    let body = std::str::from_utf8(key).unwrap().trim_start_matches('\u{feff}');
    let mut r = csv::ReaderBuilder::new().delimiter(b';').has_headers(false).from_reader(body.as_bytes());
    let mut out = "\u{feff}".as_bytes().to_vec();
    {
        let mut w = csv::WriterBuilder::new().delimiter(delim).from_writer(&mut out);
        for rec in r.records() {
            w.write_record(&rec.unwrap()).unwrap();
        }
        w.flush().unwrap();
    }
    out
}

/// Повреждённый ключ при «загрузить и записать в тот же файл»: ошибка, ключ не тронут байт в байт,
/// ни результата, ни копии, ни временного файла с настоящими значениями.
fn assert_refused(d: &Path, input: &Path, key: &Path, what: &str) {
    let before = std::fs::read(key).unwrap();
    let output = d.join(format!("out_{}.xlsx", what.replace(|c: char| !c.is_alphanumeric(), "_")));
    let err = match go(input, &output, Some(key), key, "второй") {
        Ok((_, log)) => panic!("{what}: повреждённый ключ принят, журнал: {log:?}"),
        Err(e) => format!("{e:#}"),
    };
    assert!(err.contains("НЕ изменён"), "{what}: в ошибке не сказано, что ключ цел: {err}");
    assert!(err.contains("«;»"), "{what}: в ошибке нет подсказки, как сохранить ключ: {err}");
    assert_eq!(std::fs::read(key).unwrap(), before, "{what}: ключ изменён");
    assert!(!output.exists(), "{what}: результат записан без ключа");
    assert!(!d.join("КЛЮЧ_ОБЕЗЛИЧИВАНИЯ_НЕ_ПЕРЕДАВАТЬ_копия.csv").exists(), "{what}: копия поверх рабочей");
    assert!(!d.join("КЛЮЧ_ОБЕЗЛИЧИВАНИЯ_НЕ_ПЕРЕДАВАТЬ.csv.tmp").exists(), "{what}: остался временный ключ");
}

/// Ключ пересохранён с «,» (Excel с английскими настройками, Google Таблицы): загружался как 0 записей,
/// второй прогон выдавал новые фейки и затирал ими ключ. Заголовок узнаётся при «,» — ключ читается.
#[test]
fn k01_comma_separated_key_is_read() {
    let d = tmp("k01");
    let input = book(&d);
    let key = d.join(KEY);
    go(&input, &d.join("out1.xlsx"), None, &key, "первый").unwrap();
    let first = fakes(&d.join("out1.xlsx"));
    let entries = std::fs::read_to_string(&key).unwrap().lines().count() - 1;
    for (what, delim) in [("запятая", b','), ("табуляция", b'\t')] {
        std::fs::write(&key, resave(&std::fs::read(&key).unwrap(), delim)).unwrap();
        let output = d.join(format!("out_{what}.xlsx"));
        let (_, log) = go(&input, &output, Some(&key), &key, "второй").unwrap();
        assert!(log.contains(&format!("Загружен ключ: {entries} записей")), "{what}: {log:?}");
        assert_eq!(fakes(&output), first, "{what}: фейки не из ключа");
        let back = std::fs::read_to_string(&key).unwrap();
        assert!(back.starts_with("\u{feff}Тип;ID;Реальное значение;Фейковое значение"), "{what}: ключ записан не в своём формате");
    }
    // Проверка самой проверки: без ключа соль «второй» даёт другие фейки.
    go(&input, &d.join("out_bez.xlsx"), None, &d.join("k_bez.csv"), "второй").unwrap();
    assert_ne!(fakes(&d.join("out_bez.xlsx")), first);
}

/// Ключ, который не узнаётся: другой разделитель, потерянный заголовок, не UTF-8 (Excel «CSV» в
/// Windows-1251) — ошибка с подсказкой, ключ не меняется.
#[test]
fn k02_unrecognized_key_is_refused() {
    let d = tmp("k02");
    let input = book(&d);
    let key = d.join(KEY);
    go(&input, &d.join("out1.xlsx"), None, &key, "первый").unwrap();
    let good = std::fs::read(&key).unwrap();
    let text = std::str::from_utf8(&good).unwrap();
    let variants: [(&str, Vec<u8>); 3] = [
        ("разделитель |", resave(&good, b'|')),
        ("без заголовка", text.lines().skip(1).collect::<Vec<_>>().join("\n").into_bytes()),
        ("не UTF-8", text.replace("Иванов", "\u{0}").into_bytes().into_iter().map(|b| if b == 0 { 0xC8 } else { b }).collect()),
    ];
    for (what, bytes) in variants {
        std::fs::write(&key, &bytes).unwrap();
        assert_refused(&d, &input, &key, what);
    }
}

/// Одна испорченная строка (ячейку стёрли, строку разрезали) молча пропускалась — её соответствие
/// пропадало при записи. Теперь — ошибка с номером строки, ключ не меняется.
#[test]
fn k03_key_with_broken_row_is_refused() {
    let d = tmp("k03");
    let input = book(&d);
    let key = d.join(KEY);
    go(&input, &d.join("out1.xlsx"), None, &key, "первый").unwrap();
    let text = std::fs::read_to_string(&key).unwrap();
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    // Последняя запись без фейка: «Тип;ID;Реальное значение».
    let last = lines.len() - 1;
    let cut = lines[last].rfind(';').unwrap();
    lines[last].truncate(cut);
    std::fs::write(&key, lines.join("\r\n") + "\r\n").unwrap();
    assert_refused(&d, &input, &key, "строка без фейка");
    let err = format!("{:#}", go(&input, &d.join("o.xlsx"), Some(&key), &key, "второй").unwrap_err());
    assert!(err.contains(&format!("строка {}", last + 1)), "нет номера строки: {err}");
}

/// Перед перезаписью загруженного ключа рядом остаётся его копия «_копия.csv» — одна, обновляется
/// каждым запуском; временного файла с настоящими значениями не остаётся.
#[test]
fn k04_overwritten_key_leaves_copy() {
    let d = tmp("k04");
    let input = book(&d);
    let key = d.join(KEY);
    let copy = d.join("КЛЮЧ_ОБЕЗЛИЧИВАНИЯ_НЕ_ПЕРЕДАВАТЬ_копия.csv");
    go(&input, &d.join("out1.xlsx"), None, &key, "первый").unwrap();
    assert!(!copy.exists(), "первый прогон: копировать нечего");
    for n in 2..=3 {
        // Новая строка — ключ меняется от прогона к прогону.
        let mut b = Book::default();
        b.sheet("Л").extend([(1, 0, s("ФИО")), (1, 1, s("Адрес")), (2, 0, s(&format!("Сидоров{n} Пётр Петрович"))), (2, 1, s("г. Тверь, д. 2"))]);
        let extra = d.join(format!("in{n}.xlsx"));
        write_xlsx(&extra, &b);
        let before = std::fs::read(&key).unwrap();
        let (_, log) = go(&extra, &d.join(format!("out{n}.xlsx")), Some(&key), &key, "первый").unwrap();
        assert_eq!(std::fs::read(&copy).unwrap(), before, "прогон {n}: копия — не ключ до прогона");
        assert_ne!(std::fs::read(&key).unwrap(), before, "прогон {n}: ключ не дописан");
        assert!(log.iter().any(|m| m.contains("_копия.csv")), "прогон {n}: в журнале нет копии: {log:?}");
        assert!(!d.join("КЛЮЧ_ОБЕЗЛИЧИВАНИЯ_НЕ_ПЕРЕДАВАТЬ.csv.tmp").exists());
    }
    assert!(!d.join("КЛЮЧ_ОБЕЗЛИЧИВАНИЯ_НЕ_ПЕРЕДАВАТЬ_прежний.csv").exists(), "загруженный ключ не откладывается в «прежний»");
}

/// Сбой записи ключа (на месте ключа — папка, файл занят): временный файл с настоящими значениями
/// оставался рядом.
#[test]
fn k05_failed_key_write_leaves_no_tmp() {
    let d = tmp("k05");
    let key = d.join("ключ.csv");
    std::fs::create_dir(&key).unwrap();
    std::fs::write(key.join("внутри.txt"), "x").unwrap();
    let entry = finboard_anon::fake::KeyEntry { kind_key: "ФИО".into(), id: None, real: "Иванов Иван".into(), fake: "Петров Пётр".into() };
    assert!(finboard_anon::keyfile::write(&key, &[entry]).is_err());
    assert!(!d.join("ключ.csv.tmp").exists(), "остался временный ключ с настоящими значениями");
}
