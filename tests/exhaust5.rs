//! Доработки 0.5.0 по отчёту аудитора: фейки номеров, когда свободных номеров той же длины не осталось.
//! Десять тысяч пятизначных телефонов «10000»–«19999» — это все номера на «1»: генератор делал по 20 000
//! случайных попыток на значение (три минуты), а затем отдавал первую — реальный номер другой строки,
//! и проверка результата этого не видела. Ещё — отмена при сборе значений и связь в ключе, поправленном
//! вручную. Каждый тест падал до исправления.

mod common;

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

use common::*;
use finboard_anon::anon::{run_batch, Batch, FileJob, Report};
use finboard_anon::fake::{phone_key, Faker, KeyEntry};
use finboard_anon::profile::{Kind, Profile, Rule, Settings};

fn settings(salt: &str) -> Settings {
    Settings { salt: salt.into(), ..Settings::default() }
}

/// Сборка с оптимизацией — пять секунд (до исправления — минуты); отладочная — с запасом.
fn limit() -> Duration {
    Duration::from_secs(if cfg!(debug_assertions) { 120 } else { 5 })
}

fn digits(s: &str) -> String {
    s.chars().filter(char::is_ascii_digit).collect()
}

/// Фейки всех значений одним генератором: ни один не равен реальному номеру, все разные, быстро.
fn check_exhausted(kind: Kind, with_id: bool, reals: &[String]) {
    let mut f = Faker::new(&settings("исчерпание"));
    let mut r = Rule::new("Л", 0, 2, None, kind);
    r.with_id = with_id;
    f.note_reals(reals.iter().map(String::as_str));
    let t = Instant::now();
    let fakes: Vec<String> = reals.iter().map(|x| f.fake(&r, x, true)).collect();
    let took = t.elapsed();
    assert!(took < limit(), "{kind:?} (ID: {with_id}): {} значений — {took:?}", reals.len());
    let real: HashSet<String> = reals.iter().map(|x| phone_key(x)).collect();
    for (x, y) in reals.iter().zip(&fakes) {
        assert!(!real.contains(&phone_key(y)), "{kind:?} (ID: {with_id}): {x} → {y} — фейк равен реальному номеру");
        assert!(digits(y).len() >= digits(x).len(), "{x} → {y}");
    }
    let uniq: HashSet<String> = fakes.iter().map(|y| digits(y)).collect();
    assert_eq!(uniq.len(), fakes.len(), "{kind:?} (ID: {with_id}): разным номерам достался один фейк");
}

/// Пятизначные телефоны «10000»–«19999»: все номера на «1» реальные — фейк длиннее, но ни с чем не совпадает.
#[test]
fn x01_short_phones_exhausted() {
    let reals: Vec<String> = (10_000..20_000).map(|n| n.to_string()).collect();
    for with_id in [false, true] {
        check_exhausted(Kind::Phone, with_id, &reals);
    }
}

/// Пятизначные номера «Счёт»: десять тысяч («10000»–«19999») и все девяносто тысяч пятизначных номеров —
/// во втором случае свободных фейков той же длины нет совсем.
#[test]
fn x02_short_accounts_exhausted() {
    let ten: Vec<String> = (10_000..20_000).map(|n| n.to_string()).collect();
    let all: Vec<String> = (10_000..100_000).map(|n| n.to_string()).collect();
    for with_id in [false, true] {
        check_exhausted(Kind::Account, with_id, &ten);
        check_exhausted(Kind::Account, with_id, &all);
    }
}

fn phone_book(path: &Path, phones: &[String]) {
    let mut b = Book::default();
    let sh = b.sheet("Л");
    sh.push((1, 0, s("Телефон")));
    for (i, p) in phones.iter().enumerate() {
        sh.push((2 + i as u32, 0, s(p)));
    }
    write_xlsx(path, &b);
}

fn run(input: &Path, output: &Path, rules: Vec<Rule>, salt: &str, key_in: Option<&Path>, key_out: Option<&Path>) -> Report {
    let b = Batch {
        files: vec![FileJob { input: input.to_path_buf(), output: output.to_path_buf(), rules }],
        settings: settings(salt),
        key_in: key_in.map(Path::to_path_buf),
        key_out: key_out.map(Path::to_path_buf),
    };
    run_batch(&b, &mut |_| {}).unwrap().remove(0)
}

/// Целиком через обработку: в результате ни одного реального номера, все фейки разные, в журнале — одна
/// строка о том, что фейки длиннее (без самих номеров).
#[test]
fn x03_exhausted_phones_end_to_end() {
    let d = tmp("x5_03");
    let reals: Vec<String> = (10_000..20_000).map(|n| n.to_string()).collect();
    let input = d.join("in.xlsx");
    phone_book(&input, &reals);
    let out = d.join("out.xlsx");
    let mut rule = Rule::new("Л", 0, 2, None, Kind::Phone);
    rule.with_id = false;
    let t = Instant::now();
    let rep = run(&input, &out, vec![rule.clone()], "x03", None, None);
    assert!(t.elapsed() < limit() * 2, "{:?}", t.elapsed());
    let g = read_book(&out);
    let real: HashSet<&str> = reals.iter().map(String::as_str).collect();
    let mut seen = HashSet::new();
    for i in 0..reals.len() {
        let v = text(&g, "Л", &format!("A{}", i + 2));
        assert!(!real.contains(v.as_str()), "строка {}: фейк {v} — реальный номер", i + 2);
        assert!(seen.insert(v.clone()), "фейк {v} достался двум номерам");
    }
    let w: Vec<&String> = rep.warnings.iter().filter(|w| w.contains("фейк длиннее настоящего")).collect();
    assert_eq!(w.len(), 1, "{:?}", rep.warnings);
    assert!(w[0].contains(": 10000."), "{}", w[0]);
    let summary = rep.summary(&Profile { rules: vec![rule], ..Profile::default() }).join("\n");
    assert!(summary.contains("не найдено"), "{summary}");
}

/// «Прервать и закрыть» во время сбора значений: прежде сбор и выбор фейков шли до конца (на исчерпанных
/// номерах — минуты), и только потом обработка останавливалась. Теперь — сразу, с ошибкой отмены.
#[test]
fn x04_cancel_during_collect() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    let d = tmp("x5_04");
    let reals: Vec<String> = (10_000..20_000).map(|n| n.to_string()).collect();
    let input = d.join("in.xlsx");
    phone_book(&input, &reals);
    let out = d.join("out.xlsx");
    let b = Batch {
        files: vec![FileJob { input, output: out.clone(), rules: vec![Rule::new("Л", 0, 2, None, Kind::Phone)] }],
        settings: settings("x04"),
        key_in: None,
        key_out: Some(d.join("key.csv")),
    };
    let flag = Arc::new(AtomicBool::new(false));
    let f2 = flag.clone();
    let t = Instant::now();
    let (res, log) = std::thread::spawn(move || {
        finboard_anon::anon::track_cancel(Some(f2.clone()));
        let mut log = Vec::new();
        let res = run_batch(&b, &mut |m| {
            if m.starts_with("Сбор значений") {
                f2.store(true, Ordering::Relaxed);
            }
            log.push(m);
        })
        .map(|_| ())
        .map_err(|e| format!("{e:#}"));
        (res, log)
    })
    .join()
    .unwrap();
    let err = res.expect_err("отмена не остановила обработку");
    assert!(err.contains("прервано"), "{err}");
    assert!(!log.iter().any(|m| m.starts_with("Новых значений")), "фейки выбирались после отмены: {log:?}");
    assert!(t.elapsed() < limit(), "{:?}", t.elapsed());
    assert!(!out.exists() && !d.join("key.csv").exists());
}

/// Ключ, поправленный вручную: «ФИО@Контрагент» с заглавной. Связь правила «Контрагент» — «контрагент»,
/// и записи ключа уходили в другой словарь: тот же человек получал новый фейк.
#[test]
fn x05_key_link_is_normalized() {
    let mut r = Rule::new("Л", 0, 2, None, Kind::FullName);
    r.with_id = false;
    r.link = "Контрагент".into();
    let head = Rule { link: String::new(), ..r.clone() }.kind_key();
    let mut f = Faker::new(&settings("x05"));
    f.load_entries(vec![KeyEntry {
        kind_key: format!("{head}@Контрагент"),
        id: None,
        real: "Иванов Иван Иванович".into(),
        fake: "Петров Пётр Петрович".into(),
    }]);
    assert_eq!(f.entries[0].kind_key, r.kind_key(), "связь записи ключа не приведена к виду правила");
    assert_eq!(f.fake(&r, "Иванов Иван Иванович", false), "Петров Пётр Петрович");
    // Словари той же связи: однофамилица из ключа получает ту же фейковую фамилию.
    let w = f.fake(&r, "Иванова Мария Сергеевна", false);
    assert!(w.starts_with("Петрова "), "{w}");
}

/// Фейк из ключа прошлых запусков совпал с реальным номером, появившимся в данных позже: в результате
/// реальный номер стоит в чужой строке. Проверка исключала всё, что записано как фейк, и говорила «не найдено».
#[test]
fn x06_key_fake_equal_to_real_is_reported() {
    let d = tmp("x5_06");
    let mut rule = Rule::new("Л", 0, 2, None, Kind::Phone);
    rule.with_id = false;
    let key = d.join("key.csv");
    let a = "+7 916 111-22-33".to_string();
    let in1 = d.join("in1.xlsx");
    phone_book(&in1, std::slice::from_ref(&a));
    let out1 = d.join("out1.xlsx");
    run(&in1, &out1, vec![rule.clone()], "x06", None, Some(&key));
    let fake_a = text(&read_book(&out1), "Л", "A2");
    // Второй файл: тот же номер и новый — реальный номер, равный фейку первого.
    let b = format!("8{}", phone_key(&fake_a));
    let in2 = d.join("in2.xlsx");
    phone_book(&in2, &[a.clone(), b.clone()]);
    let out2 = d.join("out2.xlsx");
    let rep = run(&in2, &out2, vec![rule.clone()], "x06", Some(&key), None);
    let g = read_book(&out2);
    assert_eq!(text(&g, "Л", "A2"), fake_a, "фейк из ключа не сохранился");
    assert_ne!(phone_key(&text(&g, "Л", "A3")), phone_key(&b), "новый номер получил фейк, равный себе");
    assert!(!rep.leaks.is_empty(), "фейк, равный реальному номеру, не попал в находки проверки");
    assert!(rep.leaks.iter().any(|l| l.samples.contains(&phone_key(&b))), "{:?}", rep.leaks);
    let summary = rep.summary(&Profile { rules: vec![rule], ..Profile::default() }).join("\n");
    assert!(!summary.contains("не найдено"), "{summary}");
    assert!(!summary.contains(&phone_key(&b)), "реальный номер в журнале: {summary}");
}
