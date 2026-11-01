//! Доработки 0.5.0 по отчёту аудитора: ключ и замок. Ключ, пересохранённый в русском Excel (числа испорчены),
//! результаты «A.xlsx» и «a.xlsx» в одном пакете, замок после перезагрузки (номер процесса занят снова),
//! снятие чужого замка, сбой записи ключа после откладывания прежнего, временный файл ключа от прерванного
//! запуска. Каждый тест падал до исправления.

mod common;

use std::path::{Path, PathBuf};

use common::*;
use finboard_anon::anon::{run_batch, Batch, FileJob};
use finboard_anon::keyfile::{lock, lock_path, tmp_path};
use finboard_anon::profile::{Kind, Rule, Settings};

const KEY: &str = "КЛЮЧ_ОБЕЗЛИЧИВАНИЯ_НЕ_ПЕРЕДАВАТЬ.csv";
const HEADER: &str = "\u{feff}Тип;ID;Реальное значение;Фейковое значение\r\n";

fn rules() -> Vec<Rule> {
    vec![Rule::new("Л", 0, 2, None, Kind::Inn), Rule::new("Л", 1, 2, None, Kind::Account)]
}

/// Книга с номерами, которые портит Excel: ИНН из 12 цифр, счёт из 20 цифр, номер с ведущими нулями.
fn numbers_book(path: &Path) {
    let mut b = Book::default();
    b.sheet("Л").extend([
        (1, 0, s("ИНН")),
        (1, 1, s("Счёт")),
        (2, 0, s("770708389312")),
        (2, 1, s("40702810100000001234")),
        (3, 0, s("500100732259")),
        (3, 1, s("0012345678")),
    ]);
    write_xlsx(path, &b);
}

/// Небольшая книга — для проверок записи (замок, имена результатов).
fn small_book(path: &Path, who: &str) {
    let mut b = Book::default();
    b.sheet("Л").extend([(1, 0, s("ИНН")), (1, 1, s("Счёт")), (2, 0, s(who)), (2, 1, s("40702810100000009876"))]);
    write_xlsx(path, &b);
}

fn batch(files: Vec<(PathBuf, PathBuf)>, key_in: Option<&Path>, key_out: Option<&Path>) -> Batch {
    Batch {
        files: files.into_iter().map(|(input, output)| FileJob { input, output, rules: rules() }).collect(),
        settings: Settings { salt: "к5".into(), ..Settings::default() },
        key_in: key_in.map(Into::into),
        key_out: key_out.map(Into::into),
    }
}

fn run(b: &Batch) -> anyhow::Result<Vec<String>> {
    let mut log = Vec::new();
    run_batch(b, &mut |m| log.push(m))?;
    Ok(log)
}

/// Что делает русский Excel с ячейкой при «Сохранить как CSV»: чисто цифровое значение становится числом —
/// ведущие нули срезаются, от 12 цифр — «7,70708E+11».
fn excel_cell(v: &str) -> String {
    if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
        return v.to_string();
    }
    let d = v.trim_start_matches('0');
    if d.is_empty() {
        return "0".into();
    }
    if d.len() <= 11 {
        return d.to_string();
    }
    let frac = d[1..6].trim_end_matches('0');
    let sep = if frac.is_empty() { "" } else { "," };
    format!("{}{sep}{frac}E+{:02}", &d[..1], d.len() - 1)
}

/// Ключ, открытый и сохранённый в русском Excel: те же строки, значения — как их сохраняет Excel.
fn excel_resave(key: &Path) {
    let text = std::fs::read_to_string(key).unwrap();
    let mut r = csv::ReaderBuilder::new().delimiter(b';').has_headers(false).from_reader(text.trim_start_matches('\u{feff}').as_bytes());
    let mut out = "\u{feff}".as_bytes().to_vec();
    {
        let mut w = csv::WriterBuilder::new().delimiter(b';').from_writer(&mut out);
        for (i, rec) in r.records().enumerate() {
            let rec = rec.unwrap();
            let row: Vec<String> = rec.iter().enumerate().map(|(c, v)| if i > 0 && c >= 2 { excel_cell(v) } else { v.to_string() }).collect();
            w.write_record(&row).unwrap();
        }
        w.flush().unwrap();
    }
    std::fs::write(key, out).unwrap();
}

/// Ключ из Excel молча принимался: «7,70708E+11» не совпадал ни с одним ИНН, следующий прогон выдавал новые
/// фейки, а испорченные строки оставались в ключе навсегда. Теперь — ошибка с номером строки, ключ не тронут,
/// настоящий номер в сообщении не показан.
#[test]
fn k5_01_key_resaved_by_excel_is_refused() {
    let d = tmp("k5_01");
    let (input, key) = (d.join("in.xlsx"), d.join(KEY));
    numbers_book(&input);
    run(&batch(vec![(input.clone(), d.join("out1.xlsx"))], None, Some(&key))).unwrap();
    // Проверка самой проверки: целый ключ читается.
    finboard_anon::keyfile::read(&key).expect("целый ключ не читается");
    excel_resave(&key);
    let before = std::fs::read(&key).unwrap();
    let out = d.join("out2.xlsx");
    let err = format!("{:#}", run(&batch(vec![(input.clone(), out.clone())], Some(&key), Some(&key))).expect_err("испорченный ключ принят"));
    assert!(err.contains("пересохранён в Excel"), "{err}");
    assert!(err.contains("НЕ изменён") && err.contains("_копия.csv"), "{err}");
    assert!(err.contains("строка 2"), "нет номера строки: {err}");
    for real in ["770708389312", "770708", "40702810100000001234", "4070281", "500100", "12345678"] {
        assert!(!err.contains(real), "в сообщении настоящий номер {real}: {err}");
    }
    assert_eq!(std::fs::read(&key).unwrap(), before, "ключ изменён");
    assert!(!out.exists(), "результат записан");
    assert!(!tmp_path(&key).exists() && !lock_path(&key).exists());
}

/// Каждый признак порчи по отдельности — и записи, которые на порчу похожи, но верны.
#[test]
fn k5_02_excel_damage_signs() {
    let d = tmp("k5_02");
    let key = d.join("ключ.csv");
    let read = |rows: &str| {
        std::fs::write(&key, format!("{HEADER}{rows}")).unwrap();
        finboard_anon::keyfile::read(&key).map_err(|e| format!("{e:#}"))
    };
    let bad = [
        // Фейк в экспоненциальной записи, с запятой и с точкой (Excel с английскими настройками).
        ("ИНН;;770708389312;9,90001E+11\r\n", "9,90001E+11"),
        ("ИНН;;770708389312;9.90001E+11\r\n", "9.90001E+11"),
        // Испорчен только настоящий: показывается без цифр.
        ("ИНН;;7,70708E+11;990001234567\r\n", "#,#####E+11"),
        // Фейк потерял ведущие нули: «0000000012» → «12».
        ("Номер (счёт/карта/договор)+ID;12;0012345678;12\r\n", "«12»"),
        // Настоящий потерял ведущие нули, фейк — нет.
        ("СНИЛС;;1234567890;100-000-012 34\r\n", "*** 90"),
        ("Телефон;;9161234567;09001234567\r\n", "*** 67"),
    ];
    for (row, shown) in bad {
        let err = read(row).expect_err(&format!("принята испорченная запись {row}"));
        assert!(err.contains("пересохранён в Excel") && err.contains("строка 2") && err.contains(shown), "{row}: {err}");
        assert!(!err.contains("770708") && !err.contains("1234567890") && !err.contains("9161234567"), "{row}: в сообщении настоящий номер: {err}");
    }
    let good = [
        "ИНН;;7707083893;9900012345\r\n",
        // ИНН с ведущим нулём, срезанным ещё в исходной книге: фейк на цифру длиннее.
        "ИНН;;274062111;9900012345\r\n",
        // Короткий код: фейк длиннее.
        "Номер (счёт/карта/договор);;Дог4659;Дог934116\r\n",
        // Счёт как текст с ведущими нулями.
        "Номер (счёт/карта/договор)+ID;12;0012345678;0000000012\r\n",
        // Число с точкой и многими цифрами — так оно записано в xlsx, это не Excel-показ.
        "Номер (счёт/карта/договор);;1.2345678901234567E+19;1.2345000000000000E+19\r\n",
        // Текст, похожий на число: не номер.
        "ФИО;;Иванов Иван;Петров Пётр\r\n",
    ];
    for row in good {
        read(row).unwrap_or_else(|e| panic!("отвергнута верная запись {row}: {e}"));
    }
    // Ключ версии 0.4.0 читается как прежде.
    let v04 = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("data").join("q5_v04_key.csv");
    finboard_anon::keyfile::read(&v04).expect("ключ 0.4.0 не читается");
}

/// «--out A.xlsx --out a.xlsx» (файлов ещё нет): пути сравнивались с учётом регистра, а в Windows это один файл —
/// второй временный файл затирал первый, и «A.xlsx» получал данные другого файла. Теперь — ошибка, ничего не записано.
#[cfg(windows)]
#[test]
fn k5_03_outputs_differing_in_case_are_refused() {
    let d = tmp("k5_03");
    let key = d.join(KEY);
    let (in1, in2) = (d.join("in1.xlsx"), d.join("in2.xlsx"));
    small_book(&in1, "770708389312");
    small_book(&in2, "500100732259");
    for (o1, o2) in [(d.join("A.xlsx"), d.join("a.xlsx")), (d.join("Б.xlsx"), d.join("sub").join("..").join("б.XLSX"))] {
        std::fs::create_dir_all(d.join("sub")).unwrap();
        let err = format!("{:#}", run(&batch(vec![(in1.clone(), o1.clone()), (in2.clone(), o2.clone())], None, Some(&key))).expect_err("два результата в один файл"));
        assert!(err.contains("в один результат"), "{err}");
        let names: Vec<String> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names.len(), 3, "что-то записано: {names:?}");
    }
    // Результат совпадает с исходным с точностью до регистра.
    let err = format!("{:#}", run(&batch(vec![(in1.clone(), d.join("IN1.XLSX"))], None, Some(&key))).unwrap_err());
    assert!(err.contains("совпадает с исходным"), "{err}");
}

/// Свой замок, только время запуска процесса — другое.
fn lock_with(key: &Path, pid: u32, start: Option<&str>) {
    let host = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_default();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let start = start.map(|s| format!("процесс_запущен={s}\r\n")).unwrap_or_default();
    std::fs::write(lock_path(key), format!("pid={pid}\r\nкомпьютер={host}\r\nначат={now}\r\n{start}")).unwrap();
}

/// Замок, оставшийся с прошлой загрузки компьютера: номер процесса снова занят (этой же программой или другой) —
/// ключ считался занятым до 12 часов. Теперь сверяется время запуска процесса: другое — замок брошен.
#[test]
fn k5_04_lock_of_reused_pid_is_stale() {
    let d = tmp("k5_04");
    let key = d.join(KEY);
    let input = d.join("in.xlsx");
    small_book(&input, "770708389312");
    // Номер — наш, время запуска — не наше (замок до перезагрузки).
    lock_with(&key, std::process::id(), Some("1"));
    run(&batch(vec![(input.clone(), d.join("out1.xlsx"))], None, Some(&key))).expect("замок прежней загрузки не убран");
    assert!(!lock_path(&key).exists());
    // Живой чужой процесс, запущенный позже замка.
    let mut child = if cfg!(windows) {
        std::process::Command::new("ping").args(["-n", "30", "127.0.0.1"]).stdout(std::process::Stdio::null()).spawn().unwrap()
    } else {
        std::process::Command::new("sleep").arg("30").spawn().unwrap()
    };
    lock_with(&key, child.id(), Some("1"));
    let res = run(&batch(vec![(input.clone(), d.join("out2.xlsx"))], Some(&key), Some(&key)));
    // Без времени запуска (замок версии до 0.5.0) живой процесс — по-прежнему владелец.
    lock_with(&key, child.id(), None);
    let busy = run(&batch(vec![(input.clone(), d.join("out3.xlsx"))], Some(&key), Some(&key)));
    let _ = child.kill();
    let _ = child.wait();
    res.expect("замок чужого процесса с тем же номером не убран");
    let err = format!("{:#}", busy.expect_err("замок живого процесса снят"));
    assert!(err.starts_with("Ключ сейчас занят"), "{err}");
    std::fs::remove_file(lock_path(&key)).unwrap();
    // Замок, поставленный этим процессом (второй прогон в том же окне), — занят.
    let held = lock(&key).unwrap();
    let err = format!("{:#}", run(&batch(vec![(input.clone(), d.join("out4.xlsx"))], Some(&key), Some(&key))).unwrap_err());
    assert!(err.starts_with("Ключ сейчас занят"), "{err}");
    drop(held);
    assert!(!lock_path(&key).exists(), "свой замок не снят");
}

/// Свой замок при выходе удалялся, даже если на его месте уже замок другого запуска (наш сочли брошенным):
/// другой запуск оставался без замка, и третий начинал работать на том же ключе.
#[test]
fn k5_05_own_lock_drop_keeps_foreign_lock() {
    let d = tmp("k5_05");
    let key = d.join(KEY);
    let held = lock(&key).unwrap();
    let foreign = "pid=4242\r\nкомпьютер=ДРУГОЙ\r\nначат=1\r\nметка=чужая\r\n";
    std::fs::write(lock_path(&key), foreign).unwrap();
    drop(held);
    assert_eq!(std::fs::read_to_string(lock_path(&key)).unwrap(), foreign, "снят чужой замок");
    let names: Vec<String> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names.len(), 1, "лишние файлы рядом с ключом: {names:?}");
}

/// Прежний ключ (не загруженный) отложен в «_прежний», а новый не записался: ошибка не говорила, где прежний,
/// и на месте ключа было пусто. Теперь прежний возвращается на место, и сообщение об этом говорит.
#[test]
fn k5_06_failed_key_write_restores_old_key() {
    let d = tmp("k5_06");
    let key = d.join(KEY);
    let (in1, in2) = (d.join("in1.xlsx"), d.join("in2.xlsx"));
    small_book(&in1, "770708389312");
    small_book(&in2, "500100732259");
    run(&batch(vec![(in1, d.join("out1.xlsx"))], None, Some(&key))).unwrap();
    let before = std::fs::read(&key).unwrap();
    // Временный файл ключа не создаётся: на его месте папка.
    std::fs::create_dir(tmp_path(&key)).unwrap();
    std::fs::write(tmp_path(&key).join("x.txt"), "x").unwrap();
    let out2 = d.join("out2.xlsx");
    let err = format!("{:#}", run(&batch(vec![(in2, out2.clone())], None, Some(&key))).expect_err("ключ записан в папку"));
    assert!(err.contains("Прежний ключ возвращён на место") && err.contains(KEY), "{err}");
    assert!(err.contains("Файлы не записаны"), "{err}");
    assert_eq!(std::fs::read(&key).unwrap(), before, "прежний ключ не на месте");
    assert!(!d.join("КЛЮЧ_ОБЕЗЛИЧИВАНИЯ_НЕ_ПЕРЕДАВАТЬ_прежний.csv").exists(), "прежний ключ остался отложенным");
    assert!(!out2.exists(), "результат записан без ключа");
}

/// Временный файл ключа от прерванного запуска (процесс сняли) с настоящими значениями оставался рядом с ключом.
/// Теперь следующий запуск его удаляет — даже если сам потом завершится ошибкой — и пишет об этом в журнал.
#[test]
fn k5_07_leftover_key_tmp_is_removed() {
    let d = tmp("k5_07");
    let key = d.join(KEY);
    let input = d.join("in.xlsx");
    small_book(&input, "770708389312");
    let leftover = tmp_path(&key);
    std::fs::write(&leftover, format!("{HEADER}ИНН;;770708389312;990001234567\r\n")).unwrap();
    // Запуск, который падает сразу после замка (нет исходного файла).
    assert!(run(&batch(vec![(d.join("нет.xlsx"), d.join("out0.xlsx"))], None, Some(&key))).is_err());
    assert!(!leftover.exists(), "временный ключ с настоящими значениями остался");
    std::fs::write(&leftover, "остаток").unwrap();
    let log = run(&batch(vec![(input, d.join("out1.xlsx"))], None, Some(&key))).unwrap();
    assert!(log.iter().any(|m| m.starts_with("Удалён временный файл ключа")), "{log:?}");
    assert!(!leftover.exists());
}
