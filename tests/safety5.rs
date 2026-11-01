//! Доработки 0.5.0 по отчёту аудитора: запись файлов. Два запуска на одном ключе, сбой на середине пакета
//! (результат открыт в Excel), жёсткая ссылка на исходный файл, отмена обработки, номера «_2» в пакете,
//! «--no-key» в командной строке. Каждый тест падал до исправления.

mod common;

use std::path::{Path, PathBuf};

use common::*;
use finboard_anon::anon::{batch_outputs, run_batch, Batch, FileJob};
use finboard_anon::keyfile::lock_path;
use finboard_anon::profile::{Kind, Profile, Rule, Settings};

const KEY: &str = "КЛЮЧ_ОБЕЗЛИЧИВАНИЯ_НЕ_ПЕРЕДАВАТЬ.csv";

fn rules() -> Vec<Rule> {
    vec![Rule::new("Л", 0, 2, None, Kind::FullName)]
}

fn settings(salt: &str) -> Settings {
    Settings { salt: salt.into(), ..Settings::default() }
}

/// Книга с людьми: у каждого файла свои — новые люди получают в ключе новые ID.
fn book(path: &Path, people: &[&str]) {
    let mut b = Book::default();
    let sh = b.sheet("Л");
    sh.push((1, 0, s("ФИО")));
    for (i, p) in people.iter().enumerate() {
        sh.push((2 + i as u32, 0, s(p)));
    }
    write_xlsx(path, &b);
}

/// Пакет из n файлов в папке `d`: «in1.xlsx» → «out1.xlsx»…; ключ — общий ключ папки.
fn batch(d: &Path, n: usize, salt: &str) -> Batch {
    let people = ["Иванов Иван Иванович", "Петрова Мария Сергеевна", "Сидоров Пётр Николаевич", "Кузнецова Анна Павловна"];
    let files = (1..=n)
        .map(|i| {
            let input = d.join(format!("in{i}.xlsx"));
            if !input.exists() {
                book(&input, &[people[(i - 1) % people.len()], &format!("Новиков{i} Денис Андреевич")]);
            }
            FileJob { input, output: d.join(format!("out{i}.xlsx")), rules: rules() }
        })
        .collect();
    let key = d.join(KEY);
    Batch { files, settings: settings(salt), key_in: key.exists().then(|| key.clone()), key_out: Some(key) }
}

/// Временные файлы, оставшиеся в папке.
fn leftovers(d: &Path) -> Vec<String> {
    std::fs::read_dir(d)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp") || n.ends_with(".lock"))
        .collect()
}

fn run(b: &Batch) -> anyhow::Result<Vec<String>> {
    let mut log = Vec::new();
    run_batch(b, &mut |m| log.push(m))?;
    Ok(log)
}

/// Замок, будто его поставил другой идущий запуск: этот же (живой) процесс, этот компьютер, только что.
fn foreign_lock(key: &Path, pid: u32, started: u64) {
    let host = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_default();
    std::fs::write(lock_path(key), format!("pid={pid}\r\nкомпьютер={host}\r\nначат={started}\r\n")).unwrap();
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

/// Два запуска на одном ключе (два окна, окно и командная строка): оба читали один ключ, давали новым людям
/// одинаковые ID, последний затирал строки другого. Теперь второй получает «Ключ сейчас занят…»,
/// ничего не пишет и ключ не трогает.
#[test]
fn s01_second_run_on_busy_key_is_refused() {
    let d = tmp("s5_01");
    let b = batch(&d, 1, "а");
    run(&b).unwrap();
    let key = d.join(KEY);
    let before = std::fs::read(&key).unwrap();
    std::fs::remove_file(d.join("out1.xlsx")).unwrap();
    foreign_lock(&key, std::process::id(), now());
    let b = batch(&d, 2, "а");
    let err = format!("{:#}", run(&b).expect_err("запуск на занятом ключе прошёл"));
    assert!(err.starts_with("Ключ сейчас занят другим запуском"), "{err}");
    assert!(err.contains("Ничего не записано") && err.contains(".csv.lock"), "{err}");
    assert_eq!(std::fs::read(&key).unwrap(), before, "ключ изменён");
    assert!(!d.join("out1.xlsx").exists() && !d.join("out2.xlsx").exists(), "результат записан");
    assert!(lock_path(&key).exists(), "чужой замок снят");
    // Замок снят — запуск проходит, свой замок после себя не оставляет.
    std::fs::remove_file(lock_path(&key)).unwrap();
    run(&b).unwrap();
    assert!(leftovers(&d).is_empty(), "{:?}", leftovers(&d));
}

/// Брошенный замок (процесс завершился аварийно или замку больше 12 часов) не мешает: убирается сам.
#[test]
fn s02_stale_lock_is_ignored() {
    let d = tmp("s5_02");
    let key = d.join(KEY);
    // Процесс, которого уже нет.
    let mut child = std::process::Command::new(if cfg!(windows) { "cmd" } else { "true" })
        .args(if cfg!(windows) { &["/C", "exit"][..] } else { &[][..] })
        .spawn()
        .unwrap();
    let dead = child.id();
    child.wait().unwrap();
    foreign_lock(&key, dead, now());
    run(&batch(&d, 1, "б")).expect("замок мёртвого процесса не убран");
    assert!(!lock_path(&key).exists());
    // Живой процесс, но замку сутки — тоже брошен (номер процесса мог достаться другой программе).
    foreign_lock(&key, std::process::id(), now() - 24 * 3600);
    run(&batch(&d, 1, "б")).expect("суточный замок не убран");
    assert!(leftovers(&d).is_empty(), "{:?}", leftovers(&d));
}

/// Ключ изменился, пока шла обработка (запуск прежней версии без замка, правка вручную): запись затёрла бы
/// чужие строки. Теперь — ошибка, ключ остаётся таким, каким его оставил другой, результаты не пишутся.
#[test]
fn s03_key_changed_during_run_is_not_overwritten() {
    let d = tmp("s5_03");
    run(&batch(&d, 1, "в")).unwrap();
    let key = d.join(KEY);
    std::fs::remove_file(d.join("out1.xlsx")).unwrap();
    let b = batch(&d, 2, "в");
    let mut changed = Vec::new();
    let res = run_batch(&b, &mut |m| {
        // Ключ уже прочитан — другой запуск дописывает в него свою строку.
        if m.starts_with("Файл 1 из") {
            let mut t = std::fs::read(&key).unwrap();
            t.extend_from_slice("ФИО;99;Чужой Человек Иванович;Фейк Чужой\r\n".as_bytes());
            std::fs::write(&key, &t).unwrap();
            changed = t;
        }
    });
    let err = format!("{:#}", res.expect_err("изменённый ключ перезаписан"));
    assert!(err.contains("изменился") && err.contains("Ничего не записано"), "{err}");
    assert_eq!(std::fs::read(&key).unwrap(), changed, "строка другого запуска потеряна");
    assert!(!d.join("out1.xlsx").exists() && !d.join("out2.xlsx").exists(), "результат записан");
    assert!(leftovers(&d).is_empty(), "{:?}", leftovers(&d));
}

/// Результат третьего файла открыт в Excel: прежде файлы 1–2 уже были заменены (и не проверены), а третий —
/// нет. Теперь всё пишется во временные файлы и встаёт на место только целиком: ошибка называет файл и
/// просит закрыть его в Excel, прежние результаты и ключ не тронуты, временных файлов не остаётся.
#[cfg(windows)]
#[test]
fn s04_result_open_in_excel_changes_nothing() {
    use std::os::windows::fs::OpenOptionsExt;
    let d = tmp("s5_04");
    run(&batch(&d, 3, "г")).unwrap();
    let key = d.join(KEY);
    let key_before = std::fs::read(&key).unwrap();
    for i in 1..=2 {
        std::fs::write(d.join(format!("out{i}.xlsx")), format!("прежний результат {i}")).unwrap();
    }
    // Как держит открытую книгу Excel: другим можно читать, нельзя писать и заменять.
    const FILE_SHARE_READ: u32 = 1;
    let held = std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(d.join("out3.xlsx")).unwrap();
    let err = format!("{:#}", run(&batch(&d, 3, "г")).expect_err("пакет записан при открытом результате"));
    drop(held);
    assert!(err.contains("out3.xlsx") && err.contains("закройте файл в Excel"), "{err}");
    assert!(err.contains("Ничего не записано"), "{err}");
    for i in 1..=2 {
        assert_eq!(std::fs::read_to_string(d.join(format!("out{i}.xlsx"))).unwrap(), format!("прежний результат {i}"), "out{i} заменён");
    }
    assert_eq!(std::fs::read(&key).unwrap(), key_before, "ключ перезаписан");
    assert!(!d.join("КЛЮЧ_ОБЕЗЛИЧИВАНИЯ_НЕ_ПЕРЕДАВАТЬ_копия.csv").exists(), "копия ключа — хотя ничего не записано");
    assert!(leftovers(&d).is_empty(), "{:?}", leftovers(&d));
    // Файл закрыт — тот же пакет проходит.
    run(&batch(&d, 3, "г")).unwrap();
    assert!(read_book(&d.join("out1.xlsx")).contains_key("Л"));
}

/// Обработка прервана (закрытием окна) на втором файле: ни результатов, ни временных файлов, ни ключа.
#[test]
fn s05_cancel_leaves_nothing() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    let d = tmp("s5_05");
    let b = batch(&d, 3, "д");
    let flag = Arc::new(AtomicBool::new(false));
    let f2 = flag.clone();
    // Флаг отмены — свой у потока, как у потока обработки в окне.
    let res = std::thread::spawn(move || {
        finboard_anon::anon::track_cancel(Some(f2.clone()));
        run_batch(&b, &mut |m| {
            if m.starts_with("Файл 2 из 3") {
                f2.store(true, Ordering::Relaxed);
            }
        })
        .map(|_| ())
        .map_err(|e| format!("{e:#}"))
    })
    .join()
    .unwrap();
    let err = res.expect_err("отмена не остановила обработку");
    assert!(err.contains("прервано"), "{err}");
    assert!(flag.load(Ordering::Relaxed));
    for i in 1..=3 {
        assert!(!d.join(format!("out{i}.xlsx")).exists(), "out{i} записан");
    }
    assert!(!d.join(KEY).exists(), "ключ записан");
    assert!(leftovers(&d).is_empty(), "{:?}", leftovers(&d));
}

/// Номер «_2» у одинаковых имён из разных папок зависел от порядка, в котором открыты файлы: при повторном
/// прогоне «отчет_anon.xlsx» мог достаться другому исходному и затереть его прежний результат.
#[test]
fn s06_batch_numbers_do_not_depend_on_order() {
    let out = PathBuf::from("D:/результаты");
    let a = PathBuf::from("D:/клиент/март/отчет.xlsx");
    let b = PathBuf::from("D:/клиент/апрель/отчет.xlsx");
    let c = PathBuf::from("D:/клиент/итоги.xlsx");
    let one = batch_outputs(&[a.clone(), b.clone(), c.clone()], Some(&out));
    let two = batch_outputs(&[c.clone(), b.clone(), a.clone()], Some(&out));
    assert_eq!(one, [out.join("отчет_anon_2.xlsx"), out.join("отчет_anon.xlsx"), out.join("итоги_anon.xlsx")]);
    assert_eq!(two, [out.join("итоги_anon.xlsx"), out.join("отчет_anon.xlsx"), out.join("отчет_anon_2.xlsx")]);
}

/// Результат — жёсткая ссылка на исходный (другое имя того же файла): сравнение путей этого не видело.
/// Теперь — ошибка «совпадает с исходным», исходный не тронут.
#[test]
fn s07_hard_link_to_source_is_refused() {
    let d = tmp("s5_07");
    let input = d.join("исходный.xlsx");
    book(&input, &["Иванов Иван Иванович"]);
    let before = std::fs::read(&input).unwrap();
    let link = d.join("ссылка.xlsx");
    std::fs::hard_link(&input, &link).unwrap();
    let b = Batch { files: vec![FileJob { input: input.clone(), output: link.clone(), rules: rules() }], settings: settings("е"), key_in: None, key_out: None };
    let err = format!("{:#}", run(&b).expect_err("запись поверх жёсткой ссылки на исходный"));
    assert!(err.contains("совпадает с исходным"), "{err}");
    assert_eq!(std::fs::read(&input).unwrap(), before, "исходный изменён");
    assert_eq!(std::fs::read(&link).unwrap(), before);
}

/// «--no-key» в командной строке всё равно загружал найденный ключ папки — фейки шли из него. Теперь
/// «без ключа» — и на входе, и на выходе: найденный ключ не читается и не трогается, новый не пишется.
#[test]
fn s08_cli_no_key_means_no_key() {
    let d = tmp("s5_08");
    let input = d.join("in.xlsx");
    book(&input, &["Иванов Иван Иванович", "Петрова Мария Сергеевна"]);
    let profile = d.join("p.json");
    Profile { rules: rules(), settings: settings("ж") }.save(&profile).unwrap();
    let cli = env!("CARGO_BIN_EXE_finboard-anon-cli");
    let go = |out: &str, extra: &[&str]| {
        std::process::Command::new(cli)
            .args(["--profile", profile.to_str().unwrap(), "--in", input.to_str().unwrap(), "--out", d.join(out).to_str().unwrap()])
            .args(extra)
            .output()
            .unwrap()
    };
    let st = go("out1.xlsx", &[]);
    assert!(st.status.success(), "{}", String::from_utf8_lossy(&st.stderr));
    // Подменяем фейки в ключе: если ключ загрузится, они попадут в результат.
    let key = d.join(KEY);
    let text = std::fs::read_to_string(&key).unwrap();
    let marked: Vec<String> = text
        .lines()
        .enumerate()
        .map(|(i, l)| if i == 0 { l.to_string() } else { format!("{};Подставной{i}", &l[..l.rfind(';').unwrap()]) })
        .collect();
    std::fs::write(&key, marked.join("\r\n") + "\r\n").unwrap();
    let before = std::fs::read(&key).unwrap();
    let st = go("out2.xlsx", &["--no-key"]);
    assert!(st.status.success(), "{}", String::from_utf8_lossy(&st.stderr));
    let g = read_book(&d.join("out2.xlsx"));
    for a in ["A2", "A3"] {
        let v = text_of(&g, a);
        assert!(!v.contains("Подставной") && !v.is_empty(), "{a}: фейк из ключа при --no-key: {v}");
    }
    assert_eq!(std::fs::read(&key).unwrap(), before, "ключ изменён при --no-key");
    let names: Vec<String> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert!(!names.iter().any(|n| n.contains("_прежний") || n.contains("_копия")), "{names:?}");
    // Вместе с --key/--load-key — ошибка, а не молчаливый выбор одного из двух.
    let st = go("out3.xlsx", &["--no-key", "--load-key", key.to_str().unwrap()]);
    assert!(!st.status.success() && String::from_utf8_lossy(&st.stderr).contains("--no-key"));
    assert!(!d.join("out3.xlsx").exists());
}

fn text_of(g: &std::collections::HashMap<String, finboard_anon::xlsx::Grid>, a1: &str) -> String {
    text(g, "Л", a1)
}
