//! Несколько файлов заказчика: один человек, одна организация, один ИНН — один фейк во всех файлах.
//! Два способа: пакет (все файлы за один прогон) и по очереди с общим ключом.
//! Данные по форме реальных выгрузок: организация в разных написаниях, ИНН в колонке без правила,
//! «Иванов И.И.» в одном файле и полное ФИО в другом, ИП как контрагент.

mod common;

use std::collections::HashMap;
use std::path::PathBuf;

use common::*;
use finboard_anon::anon::{batch_outputs, find_key, preview_batch, run, run_batch, shared_key_path, Batch, FileJob, Job};
use finboard_anon::fake::strip_id_suffix;
use finboard_anon::profile::{Kind, Profile, Rule, Settings};
use finboard_anon::xlsx::Grid;

fn settings(salt: &str) -> Settings {
    Settings { salt: salt.into(), ..Settings::default() }
}

fn rule(sheet: &str, col: u32, kind: Kind) -> Rule {
    Rule::new(sheet, col, 2, None, kind)
}

/// Договоры: ФИО | Контрагент | ИНН (число) | Реквизиты.
fn contracts() -> (Book, Vec<Rule>) {
    let mut b = Book::default();
    let sh = b.sheet("Договоры");
    for (c, h) in ["ФИО", "Контрагент", "ИНН", "Реквизиты"].iter().enumerate() {
        sh.push((1, c as u32, s(h)));
    }
    let rows = [
        ("Иванов Иван Иванович", "ООО «Ромашка»", 7707083893.0),
        ("Петрова Анна Сергеевна", "АО \"Вектор\"", 7728168971.0),
        ("Сидоров Петр Николаевич", "ИП Козлов Олег Петрович", 500100732259.0),
    ];
    for (i, (fio, org, inn)) in rows.iter().enumerate() {
        let r = 2 + i as u32;
        sh.push((r, 0, s(fio)));
        sh.push((r, 1, s(org)));
        sh.push((r, 2, Cell::N(*inn)));
    }
    sh.push((2, 3, s("р/с 40702810900000012345")));
    let rules = vec![rule("Договоры", 0, Kind::FullName), rule("Договоры", 1, Kind::Company), rule("Договоры", 2, Kind::Inn)];
    (b, rules)
}

/// Платежи: Плательщик (другое написание) | Ответственный (инициалы) | Код (ИНН без правила) | Комментарий.
fn payments() -> (Book, Vec<Rule>) {
    let mut b = Book::default();
    let sh = b.sheet("Платежи");
    for (c, h) in ["Плательщик", "Ответственный", "Код", "Комментарий"].iter().enumerate() {
        sh.push((1, c as u32, s(h)));
    }
    let rows = [
        ("Ромашка", "Иванов И.И.", 7707083893.0, "Оплата ООО \"Ромашка\" по счёту 15"),
        ("ООО \"Ромашка\"", "Петрова Анна Сергеевна", 7728168971.0, "ИНН 7707083893, без НДС"),
        ("Общество с ограниченной ответственностью «Ромашка»", "Козлов Олег Петрович", 500100732259.0, "Сверка с Ромашка за март"),
        ("Вектор АО", "Новиков Денис Андреевич", 7710140679.0, ""),
    ];
    for (i, (org, who, code, note)) in rows.iter().enumerate() {
        let r = 2 + i as u32;
        sh.push((r, 0, s(org)));
        sh.push((r, 1, s(who)));
        sh.push((r, 2, Cell::N(*code)));
        if !note.is_empty() {
            sh.push((r, 3, s(note)));
        }
    }
    let rules = vec![rule("Платежи", 0, Kind::Company), rule("Платежи", 1, Kind::FullName)];
    (b, rules)
}

/// Название организации без формы, кавычек и ID: «ООО «Звезда» (ID 3)» → «Звезда».
fn core(s: &str) -> String {
    let s = strip_id_suffix(s);
    for (a, b) in [('«', '»'), ('"', '"')] {
        if let (Some(i), Some(j)) = (s.find(a), s.rfind(b)) {
            if j > i {
                return s[i + a.len_utf8()..j].to_string();
            }
        }
    }
    s.trim_end_matches(" АО").trim_end_matches(" ООО").to_string()
}

fn id_of(s: &str) -> Option<u32> {
    s.rsplit("ID ").next()?.strip_suffix(')')?.parse().ok()
}

struct Out {
    a: HashMap<String, Grid>,
    b: HashMap<String, Grid>,
    leaks: usize,
}

fn setup(name: &str) -> (PathBuf, PathBuf, PathBuf, Vec<Rule>, Vec<Rule>) {
    let d = tmp(name);
    let (ba, ra) = contracts();
    let (bb, rb) = payments();
    let (a, b) = (d.join("Договоры.xlsx"), d.join("Платежи.xlsx"));
    write_xlsx(&a, &ba);
    write_xlsx(&b, &bb);
    (d, a, b, ra, rb)
}

/// Пакетом: оба файла за один прогон, один ключ.
fn batch_run(name: &str) -> Out {
    let (d, a, b, ra, rb) = setup(name);
    let batch = Batch {
        files: vec![
            FileJob { input: a, output: d.join("a_anon.xlsx"), rules: ra },
            FileJob { input: b, output: d.join("b_anon.xlsx"), rules: rb },
        ],
        settings: settings("пакет"),
        key_in: None,
        key_out: Some(d.join("key.csv")),
    };
    let reps = run_batch(&batch, &mut |_| {}).unwrap();
    Out { a: read_book(&d.join("a_anon.xlsx")), b: read_book(&d.join("b_anon.xlsx")), leaks: reps.iter().map(|r| r.leaks.len()).sum() }
}

/// По очереди: сначала договоры, потом платежи с ключом первого прогона. Соль у прогонов разная —
/// как в жизни, где соль случайная на каждый запуск.
fn sequential_run(name: &str) -> Out {
    let (d, a, b, ra, rb) = setup(name);
    let key = d.join("key.csv");
    let job = |input: PathBuf, out: &str, rules: Vec<Rule>, salt: &str, key_in: bool| Job {
        input,
        output: d.join(out),
        key_in: key_in.then(|| key.clone()),
        key_out: Some(key.clone()),
        profile: Profile { rules, settings: settings(salt) },
    };
    let r1 = run(&job(a, "a_anon.xlsx", ra, "раз", false), &mut |_| {}).unwrap();
    let r2 = run(&job(b, "b_anon.xlsx", rb, "два", true), &mut |_| {}).unwrap();
    Out { a: read_book(&d.join("a_anon.xlsx")), b: read_book(&d.join("b_anon.xlsx")), leaks: r1.leaks.len() + r2.leaks.len() }
}

fn check_links(o: &Out) {
    let a = |c: &str| text(&o.a, "Договоры", c);
    let b = |c: &str| text(&o.b, "Платежи", c);

    // Организация в четырёх написаниях — одно фейковое название и один ID, форма сохранена.
    let romashka = a("B2");
    assert_ne!(core(&romashka), "Ромашка");
    for cell in ["A2", "A3", "A4"] {
        assert_eq!(core(&b(cell)), core(&romashka), "Платежи!{cell} = {} при {romashka}", b(cell));
        assert_eq!(id_of(&b(cell)), id_of(&romashka), "ID у Платежи!{cell}");
    }
    assert!(b("A3").starts_with("ООО \""), "{}", b("A3"));
    assert!(b("A4").starts_with("Общество с ограниченной ответственностью «"), "{}", b("A4"));
    assert!(!b("A2").starts_with("ООО"), "лишняя форма: {}", b("A2"));
    // «АО "Вектор"» и «Вектор АО» — одна организация.
    assert_eq!(core(&b("A5")), core(&a("B3")), "{} / {}", b("A5"), a("B3"));
    assert!(b("A5").contains(" АО"), "{}", b("A5"));

    // ИНН в колонке без правила в другом файле — тот же фейк.
    for (ca, cb) in [("C2", "C2"), ("C3", "C3"), ("C4", "C4")] {
        assert_ne!(a(ca), "7707083893");
        assert_eq!(b(cb), a(ca), "ИНН Договоры!{ca} / Платежи!{cb}");
    }
    // Внутри текста — тот же ИНН и та же организация.
    assert_eq!(b("D3"), format!("ИНН {}, без НДС", a("C2")));
    let d2 = b("D2");
    assert!(!d2.contains("Ромашка") && d2.contains(&core(&romashka)), "{d2}");
    // Название без формы и кавычек внутри текста — такого написания нет ни в одной размеченной ячейке.
    // Организация внутри фразы — всегда с ID, тем же, что в колонке.
    assert_eq!(b("D4"), format!("Сверка с {} (ID {}) за март", core(&romashka), id_of(&romashka).unwrap()));

    // Люди: «Иванов И.И.» — сокращение фейка полного ФИО из другого файла.
    let ivanov = strip_id_suffix(&a("A2")).to_string();
    let w: Vec<&str> = ivanov.split_whitespace().collect();
    let initials = format!("{} {}.{}.", w[0], w[1].chars().next().unwrap(), w[2].chars().next().unwrap());
    assert_eq!(strip_id_suffix(&b("B2")), initials, "инициалы при {ivanov}");
    assert_eq!(id_of(&b("B2")), id_of(&a("A2")));
    assert_eq!(b("B3"), a("A3"), "Петрова в обоих файлах");
    // ИП-контрагент и тот же человек в колонке ФИО — одно фейковое ФИО.
    let ip = strip_id_suffix(&a("B4")).to_string();
    assert!(ip.starts_with("ИП "), "{ip}");
    assert_eq!(&ip["ИП ".len()..], strip_id_suffix(&b("B4")), "ИП и ФИО одного человека");
    assert!(!b("B4").contains("Козлов"));

    assert_eq!(o.leaks, 0, "проверка нашла остатки");
}

#[test]
fn batch_keeps_links_between_files() {
    check_links(&batch_run("m_batch"));
}

#[test]
fn shared_key_keeps_links_between_files() {
    check_links(&sequential_run("m_seq"));
}

/// ИНН без ID: в другом файле записан иначе («77 07 083893», текстом) — фейковые цифры те же.
#[test]
fn inn_without_id_same_digits_in_any_format() {
    let d = tmp("m_inn_format");
    let key = d.join("key.csv");
    let mut ba = Book::default();
    let sh = ba.sheet("А");
    sh.push((1, 0, s("ИНН")));
    sh.push((2, 0, Cell::N(7707083893.0)));
    let mut bb = Book::default();
    let sh = bb.sheet("Б");
    sh.push((1, 0, s("ИНН")));
    sh.push((2, 0, s("77 07 083893")));
    sh.push((3, 0, s("ИНН: 7707083893")));
    write_xlsx(&d.join("a.xlsx"), &ba);
    write_xlsx(&d.join("b.xlsx"), &bb);
    let mut ra = rule("А", 0, Kind::Inn);
    ra.with_id = false;
    let mut rb = rule("Б", 0, Kind::Inn);
    rb.with_id = false;
    let job = |n: &str, r: Rule, salt: &str, key_in: bool| Job {
        input: d.join(format!("{n}.xlsx")),
        output: d.join(format!("{n}_anon.xlsx")),
        key_in: key_in.then(|| key.clone()),
        key_out: Some(key.clone()),
        profile: Profile { rules: vec![r], settings: settings(salt) },
    };
    run(&job("a", ra, "раз", false), &mut |_| {}).unwrap();
    run(&job("b", rb, "два", true), &mut |_| {}).unwrap();
    let fa = text(&read_book(&d.join("a_anon.xlsx")), "А", "A2");
    let gb = read_book(&d.join("b_anon.xlsx"));
    let digits = |s: &str| s.chars().filter(char::is_ascii_digit).collect::<String>();
    assert_ne!(fa, "7707083893");
    assert_eq!(digits(&text(&gb, "Б", "A2")), fa);
    assert!(text(&gb, "Б", "A2").starts_with(&format!("{} ", &fa[..2])), "формат с пробелами сохранён");
    assert_eq!(text(&gb, "Б", "A3"), format!("ИНН: {fa}"));
}

/// Предпросмотр второго файла пакета показывает те же фейки, что попадут в файл (паритет).
#[test]
fn batch_preview_matches_output() {
    let (d, a, b, ra, rb) = setup("m_preview");
    let st = settings("п");
    let pv = preview_batch(&[(a.clone(), ra.clone()), (b.clone(), rb.clone())], &st, None).unwrap();
    let batch = Batch {
        files: vec![
            FileJob { input: a, output: d.join("a_anon.xlsx"), rules: ra },
            FileJob { input: b, output: d.join("b_anon.xlsx"), rules: rb },
        ],
        settings: st,
        key_in: None,
        key_out: None,
    };
    run_batch(&batch, &mut |_| {}).unwrap();
    let gb = read_book(&d.join("b_anon.xlsx"));
    let gi = read_book(&batch.files[1].input);
    let mut checked = 0;
    for (ri, rp) in pv[1].iter().enumerate() {
        let r = &batch.files[1].rules[ri];
        for (orig, fake) in &rp.samples {
            let row = (2..=5).find(|&row| gi["Платежи"].get(row, r.col).is_some_and(|c| &c.text == orig)).unwrap();
            assert_eq!(&gb["Платежи"].get(row, r.col).unwrap().text, fake, "{orig}");
            checked += 1;
        }
    }
    assert!(checked >= 4);
}

/// Файлы одной папки по умолчанию пишут и читают один ключ; старый ключ рядом с файлом тоже находится.
#[test]
fn files_in_one_folder_share_key() {
    let d = tmp("m_key_path");
    let (a, b) = (d.join("a.xlsx"), d.join("b.xlsx"));
    assert_eq!(shared_key_path(&a), shared_key_path(&b));
    assert!(find_key(&b).is_none());
    let legacy = d.join("b_КЛЮЧ_НЕ_ПЕРЕДАВАТЬ.csv");
    std::fs::write(&legacy, "\u{feff}Тип;ID;Реальное значение;Фейковое значение\n").unwrap();
    assert_eq!(find_key(&b), Some(legacy));
    std::fs::write(shared_key_path(&a), "\u{feff}Тип;ID;Реальное значение;Фейковое значение\n").unwrap();
    assert_eq!(find_key(&b), Some(shared_key_path(&a)), "общий ключ папки важнее старого");
    assert_eq!(find_key(&a), Some(shared_key_path(&a)));
}

/// Случайные наборы файлов: люди, организации в разных написаниях, ИНН числом и текстом.
/// Инвариант для пакета и для очереди с ключом: одна сущность — один фейк (без формы и ID) во всех файлах,
/// фейк не совпадает ни с одним реальным значением, проверка не находит остатков.
#[test]
fn random_files_keep_entities_consistent() {
    let people = [
        "Иванов Иван Иванович", "Петрова Анна Сергеевна", "Сидоров Петр Николаевич", "Кузнецова Мария Олеговна",
        "Смирнов Алексей Викторович", "Попова Елена Андреевна", "Волков Дмитрий Игоревич", "Морозова Ольга Павловна",
    ];
    let orgs = ["Ромашка", "Вектор", "Северный ветер", "Техно Плюс", "Альтаир"];
    let inns = ["7707083893", "7728168971", "7710140679", "5029290277", "7736050003"];
    let variant = |o: &str, v: u32| match v {
        0 => format!("ООО «{o}»"),
        1 => format!("ООО \"{o}\""),
        2 => o.to_string(),
        3 => format!("{o} ООО"),
        _ => format!("Общество с ограниченной ответственностью «{o}»"),
    };
    let mut seed: u64 = 0xC0FFEE;
    let mut next = |n: u32| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((seed >> 33) % n as u64) as u32
    };
    for round in 0..6 {
        let d = tmp(&format!("m_rand_{round}"));
        // Что лежит в каждой ячейке: (файл, строка, колонка) → (сущность, исходный текст).
        let mut truth: Vec<(usize, u32, u32, String, String)> = Vec::new();
        let mut files = Vec::new();
        for f in 0..3 {
            let mut book = Book::default();
            let sh = book.sheet("Данные");
            for (c, h) in ["ФИО", "Контрагент", "ИНН", "Примечание"].iter().enumerate() {
                sh.push((1, c as u32, s(h)));
            }
            let n = 3 + next(6);
            for i in 0..n {
                let r = 2 + i;
                let p = people[next(people.len() as u32) as usize];
                let oi = next(orgs.len() as u32) as usize;
                let org = variant(orgs[oi], next(5));
                let inn = inns[oi];
                sh.push((r, 0, s(p)));
                sh.push((r, 1, s(&org)));
                if next(2) == 0 {
                    sh.push((r, 2, Cell::N(inn.parse().unwrap())));
                } else {
                    sh.push((r, 2, s(inn)));
                }
                // ИНН внутри текста в колонке без правила.
                sh.push((r, 3, s(&format!("контрагент ИНН {inn}"))));
                truth.push((f, r, 0, format!("p:{p}"), p.to_string()));
                truth.push((f, r, 1, format!("o:{}", orgs[oi]), org));
                truth.push((f, r, 2, format!("i:{inn}"), inn.to_string()));
            }
            let path = d.join(format!("f{f}.xlsx"));
            write_xlsx(&path, &book);
            let mut rules = vec![rule("Данные", 0, Kind::FullName), rule("Данные", 1, Kind::Company), rule("Данные", 2, Kind::Inn)];
            if next(2) == 0 {
                rules[2].with_id = false;
            }
            files.push((path, rules));
        }
        for mode in ["пакет", "очередь"] {
            let outs: Vec<PathBuf> = (0..3).map(|f| d.join(format!("{mode}_{f}.xlsx"))).collect();
            let key = d.join(format!("{mode}_key.csv"));
            let mut leaks = 0;
            if mode == "пакет" {
                let batch = Batch {
                    files: files.iter().zip(&outs).map(|((i, r), o)| FileJob { input: i.clone(), output: o.clone(), rules: r.clone() }).collect(),
                    settings: settings(&format!("s{round}")),
                    key_in: None,
                    key_out: Some(key.clone()),
                };
                leaks += run_batch(&batch, &mut |_| {}).unwrap().iter().map(|r| r.leaks.len()).sum::<usize>();
            } else {
                for (f, ((i, r), o)) in files.iter().zip(&outs).enumerate() {
                    let job = Job {
                        input: i.clone(),
                        output: o.clone(),
                        key_in: (f > 0).then(|| key.clone()),
                        key_out: Some(key.clone()),
                        profile: Profile { rules: r.clone(), settings: settings(&format!("s{round}-{f}")) },
                    };
                    leaks += run(&job, &mut |_| {}).unwrap().leaks.len();
                }
            }
            let grids: Vec<HashMap<String, Grid>> = outs.iter().map(|o| read_book(o)).collect();
            let mut seen: HashMap<String, String> = HashMap::new();
            let reals: Vec<&str> = truth.iter().map(|t| t.4.as_str()).chain(orgs).collect();
            for (f, r, c, entity, real) in &truth {
                let out = grids[*f]["Данные"].get(*r, *c).unwrap().text.clone();
                let norm = if entity.starts_with("o:") { core(&out) } else { strip_id_suffix(&out).to_string() };
                assert!(!reals.contains(&norm.as_str()), "{mode}, раунд {round}: {real} → {out} — реальное значение");
                let prev = seen.entry(entity.clone()).or_insert_with(|| norm.clone());
                assert_eq!(prev, &norm, "{mode}, раунд {round}: {real} → {out}, а раньше {prev}");
                if entity.starts_with("i:") {
                    let note = &grids[*f]["Данные"].get(*r, 3).unwrap().text;
                    assert_eq!(note, &format!("контрагент ИНН {norm}"), "{mode}, раунд {round}");
                }
            }
            assert_eq!(leaks, 0, "{mode}, раунд {round}");
        }
    }
}

/// Одинаковые имена файлов из разных папок не затирают друг друга в общей папке результатов.
#[test]
fn batch_outputs_are_distinct() {
    let ins = vec![PathBuf::from("C:/a/Отчет.xlsx"), PathBuf::from("C:/b/Отчет.xlsx"), PathBuf::from("C:/b/отчет.xlsx"), PathBuf::from("C:/b/План.xlsx")];
    let outs = batch_outputs(&ins, Some(std::path::Path::new("D:/out")));
    let names: Vec<String> = outs.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(names, ["Отчет_anon.xlsx", "Отчет_anon_2.xlsx", "отчет_anon_3.xlsx", "План_anon.xlsx"]);
    assert!(outs.iter().all(|p| p.starts_with("D:/out")));
    assert_eq!(batch_outputs(&ins[..1], None)[0], PathBuf::from("C:/a/Отчет_anon.xlsx"));
}

/// Прогон без загрузки ключа не затирает общий ключ папки: в нём соответствия уже отданных файлов.
#[test]
fn unloaded_shared_key_is_kept() {
    let (d, a, b, ra, rb) = setup("m_key_keep");
    let key = shared_key_path(&a);
    let job = |input: PathBuf, out: &str, rules: Vec<Rule>, key_in: Option<PathBuf>| Job {
        input,
        output: d.join(out),
        key_in,
        key_out: Some(key.clone()),
        profile: Profile { rules, settings: settings("к") },
    };
    run(&job(a, "a_anon.xlsx", ra, None), &mut |_| {}).unwrap();
    let first = std::fs::read(&key).unwrap();
    run(&job(b.clone(), "b_anon.xlsx", rb.clone(), None), &mut |_| {}).unwrap();
    let backup = d.join("КЛЮЧ_ОБЕЗЛИЧИВАНИЯ_НЕ_ПЕРЕДАВАТЬ_прежний.csv");
    assert_eq!(std::fs::read(&backup).unwrap(), first, "прежний ключ сохранён");
    // С загруженным ключом — дописывается на месте, без копий.
    run(&job(b, "b_anon.xlsx", rb, Some(key.clone())), &mut |_| {}).unwrap();
    assert!(!d.join("КЛЮЧ_ОБЕЗЛИЧИВАНИЯ_НЕ_ПЕРЕДАВАТЬ_прежний_2.csv").exists());
}

/// Значение из другого файла (через ключ) в тексте без правила: с поиском внутри текста — заменено,
/// без него — проверка сообщает об остатке (страж видит и чужие значения, не только свои).
#[test]
fn other_file_value_in_text_is_replaced_or_reported() {
    let (d, a, _, ra, _) = setup("m_leak_other");
    let key = d.join("key.csv");
    run(&Job { input: a, output: d.join("a_anon.xlsx"), key_in: None, key_out: Some(key.clone()), profile: Profile { rules: ra, settings: settings("1") } }, &mut |_| {}).unwrap();
    let mut b = Book::default();
    let sh = b.sheet("Заметки");
    sh.push((1, 0, s("Контрагент")));
    sh.push((2, 0, s("ООО «Лютик»")));
    sh.push((3, 1, s("Связаться: Иванов Иван Иванович, доб. 12")));
    let input = d.join("Заметки.xlsx");
    write_xlsx(&input, &b);
    for inner in [false, true] {
        let mut st = settings("2");
        st.inner_text = inner;
        let out = d.join(format!("b_{inner}.xlsx"));
        let job = Job { input: input.clone(), output: out.clone(), key_in: Some(key.clone()), key_out: None, profile: Profile { rules: vec![rule("Заметки", 0, Kind::Company)], settings: st } };
        let rep = run(&job, &mut |_| {}).unwrap();
        let note = text(&read_book(&out), "Заметки", "B3");
        if inner {
            assert!(rep.leaks.is_empty(), "{:?}", rep.leaks);
            assert!(!note.contains("Иванов"), "{note}");
        } else {
            assert!(rep.leaks.iter().any(|l| l.samples.iter().any(|x| x == "Иванов Иван Иванович")), "остаток не найден: {:?}", rep.leaks);
        }
    }
}

/// Консоль: несколько файлов одним вызовом и второй вызов с общим ключом папки (подхватывается сам).
#[test]
fn cli_batch_and_shared_key() {
    let (d, a, b, ra, rb) = setup("m_cli");
    let (pa, pb) = (d.join("a.json"), d.join("b.json"));
    Profile { rules: ra, settings: settings("x") }.save(&pa).unwrap();
    Profile { rules: rb, settings: settings("y") }.save(&pb).unwrap();
    let cli = env!("CARGO_BIN_EXE_finboard-anon-cli");
    let st = std::process::Command::new(cli)
        .args(["--profile", pa.to_str().unwrap(), "--in", a.to_str().unwrap(), "--out", d.join("a1.xlsx").to_str().unwrap()])
        .args(["--profile", pb.to_str().unwrap(), "--in", b.to_str().unwrap(), "--out", d.join("b1.xlsx").to_str().unwrap()])
        .output()
        .unwrap();
    assert!(st.status.success(), "{}", String::from_utf8_lossy(&st.stderr));
    assert!(shared_key_path(&a).exists());
    // Второй запуск — только платежи, ключ не указан: общий ключ папки подхватывается, фейки те же.
    let st = std::process::Command::new(cli)
        .args(["--profile", pb.to_str().unwrap(), "--in", b.to_str().unwrap(), "--out", d.join("b2.xlsx").to_str().unwrap()])
        .output()
        .unwrap();
    assert!(st.status.success(), "{}", String::from_utf8_lossy(&st.stderr));
    let (g1, g2, ga) = (read_book(&d.join("b1.xlsx")), read_book(&d.join("b2.xlsx")), read_book(&d.join("a1.xlsx")));
    for c in ["A2", "A3", "B2", "B3", "C2", "C4"] {
        assert_eq!(text(&g1, "Платежи", c), text(&g2, "Платежи", c), "{c}");
    }
    assert_eq!(core(&text(&g1, "Платежи", "A2")), core(&text(&ga, "Договоры", "B2")));
}

/// Один ИНН размечен в одном файле как «ИНН», в другом — как «Текст»: фейки разные, связка рвётся —
/// программа предупреждает, где поправить тип, и не пишет в предупреждение сами значения.
#[test]
fn different_kinds_for_same_values_are_reported() {
    let (d, a, b, ra, _) = setup("m_kinds");
    let mut rb = vec![rule("Платежи", 0, Kind::Company), Rule::new("Платежи", 2, 2, None, Kind::Text)];
    rb[1].label = "Код".into();
    let batch = Batch {
        files: vec![
            FileJob { input: a, output: d.join("a_anon.xlsx"), rules: ra },
            FileJob { input: b, output: d.join("b_anon.xlsx"), rules: rb },
        ],
        settings: settings("т"),
        key_in: None,
        key_out: None,
    };
    let reps = run_batch(&batch, &mut |_| {}).unwrap();
    for r in &reps {
        let w = r.warnings.iter().find(|w| w.contains("правилами разных типов")).unwrap_or_else(|| panic!("нет предупреждения: {:?}", r.warnings));
        assert!(w.contains("(Договоры.xlsx, лист Договоры, C)") && w.contains("(Платежи.xlsx, лист Платежи, C)"), "{w}");
        assert!(!w.contains("7707083893"), "в предупреждении реальное значение: {w}");
    }
    // Без расхождения типов — без предупреждения.
    let (d, a, b, ra, rb) = setup("m_kinds_ok");
    let batch = Batch {
        files: vec![FileJob { input: a, output: d.join("a.xlsx"), rules: ra }, FileJob { input: b, output: d.join("b.xlsx"), rules: rb }],
        settings: settings("т"),
        key_in: None,
        key_out: None,
    };
    assert!(run_batch(&batch, &mut |_| {}).unwrap().iter().all(|r| r.warnings.iter().all(|w| !w.contains("правилами разных типов"))));
}

/// Куда пишется ключ: выбранный вручную — в него; найденный при открытии — в общий ключ его папки
/// (старый ключ файла переносится в общий); иначе — в общий ключ папки первого файла.
#[test]
fn key_out_follows_found_key() {
    use finboard_anon::anon::key_out_path;
    use std::path::Path;
    let first = Path::new("D:/a/Отчет.xlsx");
    assert_eq!(key_out_path(first, None, false), shared_key_path(first));
    let other = Path::new("E:/b/КЛЮЧ_ОБЕЗЛИЧИВАНИЯ_НЕ_ПЕРЕДАВАТЬ.csv");
    assert_eq!(key_out_path(first, Some(other), false), other, "ключ второй папки не устаревает");
    let legacy = Path::new("D:/a/Отчет_КЛЮЧ_НЕ_ПЕРЕДАВАТЬ.csv");
    assert_eq!(key_out_path(first, Some(legacy), false), shared_key_path(first));
    let chosen = Path::new("F:/ключи/заказчик.csv");
    assert_eq!(key_out_path(first, Some(chosen), true), chosen);
}

/// Шапки, под которыми у заказчиков лежат контрагенты.
#[test]
fn detect_counterparty_headers() {
    for h in ["Плательщик", "Покупатель", "Заказчик", "Наименование контрагента", "Получатель платежа (организация)", "Арендатор"] {
        assert_eq!(finboard_anon::detect::kind_for_header(h), Some(Kind::Company), "{h}");
    }
}

/// Ключ: одна строка на значение, даже если оно встречается в нескольких файлах и ячейках.
/// Дубли раздувают ключ и при ручной сверке выглядят как два разных человека.
#[test]
fn key_has_one_row_per_value() {
    let d = tmp("m_key_unique");
    let mut files = Vec::new();
    for (i, name) in ["Отдел1", "Отдел2"].iter().enumerate() {
        let mut b = Book::default();
        let sh = b.sheet("Л");
        sh.push((1, 0, s("ФИО")));
        for r in 2..5 {
            sh.push((r, 0, s("Иванов Иван Иванович")));
        }
        let input = d.join(format!("{name}.xlsx"));
        write_xlsx(&input, &b);
        files.push(FileJob { input, output: d.join(format!("out{i}.xlsx")), rules: vec![rule("Л", 0, Kind::FullName)] });
    }
    let key = d.join("key.csv");
    run_batch(&Batch { files, settings: settings("д"), key_in: None, key_out: Some(key.clone()) }, &mut |_| {}).unwrap();
    let entries = finboard_anon::keyfile::read(&key).unwrap();
    let rows = entries.iter().filter(|e| e.real == "Иванов Иван Иванович").count();
    assert_eq!(rows, 1, "строк ключа на одно ФИО: {rows}");
}

/// Длинный номер под маской или «текстом» (лицевой счёт, номер договора) внутри примечания без правила.
/// Такие номера не ИНН и не телефон, поиск по цифрам их не знает — их ищет поиск значений внутри текста.
#[test]
fn masked_long_number_inside_text_is_replaced() {
    for kind in [Kind::Mask, Kind::Text] {
        let d = tmp(&format!("m_mask_in_text_{kind:?}"));
        let mut b = Book::default();
        let sh = b.sheet("Л");
        sh.push((1, 0, s("Лицевой счёт")));
        sh.push((2, 0, Cell::N(123456789012.0)));
        sh.push((3, 0, Cell::N(987654321.0)));
        sh.push((2, 2, s("Долг по л/с 123456789012 на 01.10")));
        sh.push((3, 2, s("л/с 987654321 закрыт")));
        let input = d.join("in.xlsx");
        write_xlsx(&input, &b);
        let out = d.join("out.xlsx");
        let rep = run(&Job { input, output: out.clone(), key_in: None, key_out: None, profile: Profile { rules: vec![rule("Л", 0, kind)], settings: settings("м") } }, &mut |_| {}).unwrap();
        let g = read_book(&out);
        for (a1, num) in [("C2", "123456789012"), ("C3", "987654321")] {
            let note = text(&g, "Л", a1);
            assert!(!note.contains(num), "{kind:?}: номер в примечании остался: {note}");
        }
        assert!(rep.leaks.is_empty(), "{kind:?}: {:?}", rep.leaks);
    }
}
