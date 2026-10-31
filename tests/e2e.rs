//! Сквозные тесты: настоящий xlsx на входе → автоподбор → обезличивание → чтение результата.
//! Каждый тест закрывает вид бага, найденный при ручной проверке (см. TESTING.md).

mod common;

use std::io::Read;
use std::path::Path;

use common::*;
use finboard_anon::anon::{preview, run, Job, Report};
use finboard_anon::detect;
use finboard_anon::profile::{Kind, Profile, Rule, Settings};
use finboard_anon::xlsx::{load_grid, Workbook};

fn settings(salt: &str) -> Settings {
    Settings { salt: salt.into(), ..Settings::default() }
}

/// Профиль из автоподбора по всем листам — как кнопка «Автоподбор: все листы».
fn detect_profile(path: &Path, salt: &str) -> Profile {
    let wb = Workbook::open(path).unwrap();
    let mut p = Profile { rules: Vec::new(), settings: settings(salt) };
    for (i, sh) in wb.sheets.iter().enumerate() {
        p.rules.extend(detect::suggest(&sh.name, &load_grid(path, &wb, i, 1_000_000).unwrap()));
    }
    p
}

fn anonymize(input: &Path, output: &Path, key_in: Option<&Path>, key_out: Option<&Path>, profile: &Profile) -> Report {
    let job = Job {
        input: input.into(),
        output: output.into(),
        key_in: key_in.map(Into::into),
        key_out: key_out.map(Into::into),
        profile: profile.clone(),
    };
    run(&job, &mut |_| {}).unwrap()
}

fn part(path: &Path, name: &str) -> String {
    let mut z = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut s = String::new();
    z.by_name(name).unwrap().read_to_string(&mut s).unwrap();
    s.replace("&quot;", "\"")
}

/// Ячейки всех правил: (лист, строка, колонка).
fn ruled_cells(p: &Profile, g: &std::collections::HashMap<String, finboard_anon::xlsx::Grid>) -> Vec<(String, u32, u32)> {
    let mut v = Vec::new();
    for r in &p.rules {
        let grid = &g[&r.sheet];
        for row in r.row_from..=r.row_to.unwrap_or(grid.max_row) {
            if grid.get(row, r.col).is_some() {
                v.push((r.sheet.clone(), row, r.col));
            }
        }
    }
    v
}

fn ranges(p: &Profile) -> Vec<String> {
    p.rules.iter().map(|r| format!("{}!{} {:?}", r.sheet, r.range_text(), r.kind)).collect()
}

// ---------------------------------------------------------------- край: границы таблиц

#[test]
fn detect_finds_exact_table_bounds() {
    let d = tmp("detect");
    let input = d.join("report.xlsx");
    write_xlsx(&input, &report_book());
    let p = detect_profile(&input, "s");
    assert_eq!(
        ranges(&p),
        [
            "Отчет!C10:C29 FullName",
            "Отчет!A36:A39 FullName",
            "Лист1!I15:I34 FullName",
            "Лист1!G41:G44 FullName",
        ]
    );
}

// ---------------------------------------------------------------- сквозной прогон

#[test]
fn report_end_to_end() {
    let d = tmp("e2e");
    let (input, output, key) = (d.join("report.xlsx"), d.join("out.xlsx"), d.join("key.csv"));
    write_xlsx(&input, &report_book());
    let p = detect_profile(&input, "s");
    let rep = anonymize(&input, &output, None, Some(&key), &p);
    assert!(rep.leaks.is_empty(), "остатки: {:?}", rep.leaks);
    assert_eq!(rep.rule_cells, vec![20, 4, 20, 4]);

    let (gi, go) = (read_book(&input), read_book(&output));
    // Заголовки, названия разделов, итоги и чужие колонки не тронуты.
    for (sh, a1) in [("Отчет", "C9"), ("Отчет", "A34"), ("Отчет", "A35"), ("Отчет", "A40"), ("Отчет", "B10"), ("Отчет", "C36"), ("Лист1", "I14"), ("Лист1", "G45")] {
        assert_eq!(text(&gi, sh, a1), text(&go, sh, a1), "{sh}!{a1} изменился");
    }
    // Один человек — один фейк на обоих листах и в обеих таблицах; ID без пропусков 1..4.
    let mut ids = Vec::new();
    for (i, real) in NAMES.iter().enumerate() {
        let a = text(&go, "Отчет", &format!("A{}", 36 + i));
        assert_eq!(a, text(&go, "Лист1", &format!("G{}", 41 + i)));
        assert!(!a.contains(real), "{real} → {a}");
        let surname = a.split_whitespace().next().unwrap();
        let female = real.split_whitespace().next().unwrap().ends_with('а');
        assert_eq!(surname.ends_with('а') || surname.ends_with("ая"), female, "пол: {real} → {a}");
        ids.push(a.rsplit("ID ").next().unwrap().trim_end_matches(')').parse::<u32>().unwrap());
    }
    ids.sort();
    assert_eq!(ids, vec![1, 2, 3, 4]);
    // Строка внутри формулы и имя внутри текста заменены тем же фейком.
    let petrova = text(&go, "Отчет", "A37");
    let svod = part(&output, "xl/worksheets/sheet3.xml");
    assert!(svod.contains(&format!("\"{petrova}\"")), "формула: {svod}");
    let ivanov = text(&go, "Отчет", "A36");
    assert_eq!(text(&go, "Отчет", "A42"), format!("Ответственный: {ivanov}"));
    // Автор в свойствах файла.
    assert!(!part(&output, "docProps/core.xml").contains("Иванов"));
}

/// Пользователь выделил колонку целиком, вместе с заголовком и «ИТОГО»: подписи не заменяются
/// и не забирают себе ID (раньше «Исполнитель» становился человеком с ID 1 и сдвигал остальных).
#[test]
fn labels_inside_rule_are_kept_and_take_no_id() {
    let d = tmp("labels");
    let (input, output) = (d.join("report.xlsx"), d.join("out.xlsx"));
    write_xlsx(&input, &report_book());
    let mut p = Profile { rules: Vec::new(), settings: settings("s") };
    p.rules.push(Rule::new("Отчет", 2, 1, None, Kind::FullName)); // C1:C… — с заголовком «Исполнитель»
    p.rules.push(Rule::new("Отчет", 0, 30, Some(45), Kind::FullName)); // A30:A45 — раздел, заголовок, ИТОГО
    anonymize(&input, &output, None, None, &p);
    let go = read_book(&output);
    for a1 in ["C9", "A34", "A35", "A40"] {
        assert_eq!(text(&go, "Отчет", a1), text(&read_book(&input), "Отчет", a1), "{a1}");
    }
    let mut ids: Vec<u32> = (36..=39)
        .map(|r| text(&go, "Отчет", &format!("A{r}")).rsplit("ID ").next().unwrap().trim_end_matches(')').parse().unwrap())
        .collect();
    ids.sort();
    assert_eq!(ids, vec![1, 2, 3, 4]);
}

// ---------------------------------------------------------------- паритет: предпросмотр = файл

#[test]
fn preview_matches_output_without_and_with_key() {
    let d = tmp("parity");
    let (input, out1, key) = (d.join("report.xlsx"), d.join("out1.xlsx"), d.join("key.csv"));
    write_xlsx(&input, &report_book());
    let p = detect_profile(&input, "соль-1");
    let gi = read_book(&input);

    let check = |pv: &[finboard_anon::anon::RulePreview], go: &std::collections::HashMap<String, finboard_anon::xlsx::Grid>, rep: Option<&Report>| {
        for (ri, rp) in pv.iter().enumerate() {
            let r = &p.rules[ri];
            if let Some(rep) = rep {
                assert_eq!(rp.cells, rep.rule_cells[ri], "число ячеек правила {}", r.range_text());
            }
            assert!(!rp.samples.is_empty(), "у правила {} нет примеров в предпросмотре", r.range_text());
            for (orig, fake) in &rp.samples {
                let row = (r.row_from..=r.row_to.unwrap())
                    .find(|&row| gi[&r.sheet].get(row, r.col).is_some_and(|c| &c.text == orig))
                    .unwrap();
                let got = go[&r.sheet].get(row, r.col).unwrap().text.clone();
                assert_eq!(&got, fake, "предпросмотр {orig} → {fake}, в файле {got}");
            }
        }
    };

    // Без ключа: та же соль профиля — те же фейки и те же ID.
    let pv = preview(&input, &p, None).unwrap();
    let rep = anonymize(&input, &out1, None, Some(&key), &p);
    check(&pv, &read_book(&out1), Some(&rep));

    // С ключом прошлого запуска и другой солью: предпросмотр берёт фейки из ключа.
    let p2 = Profile { settings: settings("соль-2"), ..p.clone() };
    let pv2 = preview(&input, &p2, Some(&key)).unwrap();
    check(&pv2, &read_book(&out1), None);
}

// ---------------------------------------------------------------- регресс: повтор с ключом

#[test]
fn rerun_with_key_reproduces_every_cell() {
    let d = tmp("replay");
    let (input, out1, out2, key) = (d.join("report.xlsx"), d.join("o1.xlsx"), d.join("o2.xlsx"), d.join("key.csv"));
    write_xlsx(&input, &report_book());
    let p = detect_profile(&input, "первая");
    anonymize(&input, &out1, None, Some(&key), &p);
    let p2 = Profile { settings: settings("другая"), ..p.clone() };
    anonymize(&input, &out2, Some(&key), Some(&d.join("key2.csv")), &p2);
    let (g1, g2) = (read_book(&out1), read_book(&out2));
    let cells = ruled_cells(&p, &g1);
    assert_eq!(cells.len(), 48);
    for (sh, r, c) in cells {
        assert_eq!(g1[&sh].get(r, c).unwrap().text, g2[&sh].get(r, c).unwrap().text, "{sh} {r}:{c}");
    }
}

#[test]
fn reanonymizing_output_keeps_gender_and_format() {
    let d = tmp("reanon");
    let (input, out1, out2) = (d.join("report.xlsx"), d.join("o1.xlsx"), d.join("o2.xlsx"));
    write_xlsx(&input, &report_book());
    anonymize(&input, &out1, None, None, &detect_profile(&input, "a"));
    // Второй круг по уже обезличенному файлу: автоподбор находит те же диапазоны.
    let p = detect_profile(&out1, "b");
    assert_eq!(ranges(&p), ranges(&detect_profile(&input, "x")));
    let rep = anonymize(&out1, &out2, None, None, &p);
    assert!(rep.leaks.is_empty());
    let (g1, g2) = (read_book(&out1), read_book(&out2));
    for i in 0..4 {
        let before = text(&g1, "Отчет", &format!("A{}", 36 + i));
        let after = text(&g2, "Отчет", &format!("A{}", 36 + i));
        let words: Vec<&str> = after.split_whitespace().collect();
        assert_eq!(words.len(), 4, "формат «Фамилия Имя (ID n)»: {after}");
        assert_eq!(after.matches("(ID").count(), 1, "двойной ID: {after}");
        let fem = |x: &str| {
            let w = x.split_whitespace().next().unwrap();
            w.ends_with('а') || w.ends_with("ая")
        };
        assert_eq!(fem(&before), fem(&after), "пол: {before} → {after}");
    }
}

// ---------------------------------------------------------------- страж: проверка утечек не пустая

#[test]
fn leak_scan_reports_planted_value() {
    let d = tmp("leak");
    let input = d.join("report.xlsx");
    let mut b = report_book();
    b.sheets[2].1.push((3, 0, s("Связаться: Сидоров Пётр, доб. 12")));
    write_xlsx(&input, &b);
    let mut p = detect_profile(&input, "s");

    p.settings.inner_text = false;
    let rep = anonymize(&input, &d.join("off.xlsx"), None, None, &p);
    let leak = rep.leaks.iter().find(|l| l.part == "xl/sharedStrings.xml").expect("подложенное значение не найдено");
    assert!(leak.samples.iter().any(|s| s == "Сидоров Пётр"), "{:?}", rep.leaks);

    p.settings.inner_text = true;
    let rep = anonymize(&input, &d.join("on.xlsx"), None, None, &p);
    assert!(rep.leaks.is_empty(), "{:?}", rep.leaks);
}

/// Обратная сторона стража: реальное «Иван», которое само входит в фейк «Иванова Мария (ID 2)»,
/// не должно давать ложную тревогу — иначе отчёт проверки перестают читать.
#[test]
fn leak_scan_ignores_real_value_inside_fake() {
    let d = tmp("leak-false");
    let path = d.join("fake.xlsx");
    let mut b = Book::default();
    b.sheet("Л").push((1, 0, s("Иванова Мария (ID 2)")));
    b.sheet("Л").push((2, 0, s("Звонил Иван")));
    write_xlsx(&path, &b);
    let leaks = finboard_anon::anon::scan_leaks(&path, &["Иван".to_string()], &["Иванова Мария (ID 2)"]).unwrap();
    assert!(leaks.is_empty(), "{leaks:?}");
    // А без такого фейка то же значение — настоящая находка.
    let leaks = finboard_anon::anon::scan_leaks(&path, &["Иван".to_string()], &["Петров Олег (ID 1)"]).unwrap();
    assert_eq!(leaks.len(), 1);
}

/// Проверка ищет значение отдельным словом: «Фирма1» внутри «Фирма13» — не утечка (ложная тревога у клиента),
/// а сама «Фирма13» рядом с отброшенной «Фирма1» — утечка.
#[test]
fn leak_scan_matches_whole_words() {
    let d = tmp("leak-words");
    let path = d.join("fake.xlsx");
    let mut b = Book::default();
    b.sheet("Л").push((1, 0, s("Оплата Фирма13 по счёту, ТоргФирма1")));
    write_xlsx(&path, &b);
    let scan = |orig: &[&str]| {
        let orig: Vec<String> = orig.iter().map(|s| s.to_string()).collect();
        finboard_anon::anon::scan_leaks(&path, &orig, &["Петров Олег (ID 1)"]).unwrap()
    };
    assert!(scan(&["Фирма1"]).is_empty(), "{:?}", scan(&["Фирма1"]));
    let leaks = scan(&["Фирма1", "Фирма13"]);
    assert_eq!(leaks.len(), 1, "{leaks:?}");
    assert_eq!(leaks[0].samples, vec!["Фирма13".to_string()], "{leaks:?}");
}

// ---------------------------------------------------------------- состояние: правила

/// Цепочка случайных действий с правилами (как щелчки в окне) и инварианты после каждого шага.
#[test]
fn random_rule_edits_keep_invariants() {
    let mut seed: u64 = 0x5eed;
    let mut next = |n: u32| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((seed >> 33) % n as u64) as u32
    };
    let mut p = Profile::default();
    for step in 0..400 {
        let sheet = ["Отчет", "Лист1"][next(2) as usize];
        let col = next(3);
        let from = 1 + next(40);
        let to = if next(5) == 0 { None } else { Some(from + next(30)) };
        let replace = next(3) != 0;
        let new = Rule::new(sheet, col, from, to, Kind::FullName);
        let before = p.rules.len();
        let (added, replaced) = p.add_rules(vec![new.clone()], replace);
        // Правила одной колонки никогда не пересекаются.
        for (i, a) in p.rules.iter().enumerate() {
            for b in &p.rules[i + 1..] {
                let overlap = a.sheet == b.sheet && a.col == b.col
                    && a.row_from <= b.row_to.unwrap_or(u32::MAX) && b.row_from <= a.row_to.unwrap_or(u32::MAX);
                assert!(!overlap, "шаг {step}: {} и {}", a.range_text(), b.range_text());
            }
        }
        // Выделение с заменой всегда становится правилом (сужение/расширение работает).
        if replace {
            assert!(p.rules.contains(&new), "шаг {step}: {} не применено", new.range_text());
        }
        // Новое правило, заменившее старые, входит в «заменено», а не в «добавлено».
        let inserted = if replaced > 0 { 1 } else { added };
        assert_eq!(p.rules.len(), before + inserted - replaced, "шаг {step}");
    }
}

// ---------------------------------------------------------------- страж кода

/// Фейки создаются только в библиотеке: если окно заведёт свой генератор, предпросмотр
/// снова начнёт расходиться с результатом.
#[test]
fn only_library_creates_fakes() {
    for f in ["src/main.rs", "src/bin/cli.rs"] {
        let code = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(f)).unwrap();
        for banned in ["Faker::new", "fake::Faker", ".fake("] {
            assert!(!code.contains(banned), "{f} содержит {banned}: используйте anon::plan/preview");
        }
    }
}

// ---------------------------------------------------------------- настоящий файл Excel

/// Книга, сохранённая Excel: сводная, диаграмма, формулы со строками, кэши.
#[test]
fn excel_made_dashboard() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let d = tmp("demo");
    let input = root.join("samples/demo_dashboard.xlsx");
    let mut p = Profile::load(&root.join("samples/demo_profile.json")).unwrap();
    p.settings.salt = "demo".into();
    let (o1, o2, key) = (d.join("o1.xlsx"), d.join("o2.xlsx"), d.join("key.csv"));
    let rep = anonymize(&input, &o1, None, Some(&key), &p);
    assert!(rep.leaks.is_empty(), "{:?}", rep.leaks);
    assert!(rep.sst_replaced > 0 && rep.formula_text_replaced > 0 && rep.other_replaced > 0, "{rep:?}");
    let records = part(&o1, "xl/pivotCache/pivotCacheRecords1.xml");
    assert!(!records.contains("79161234567") && !records.contains("client1@mail.ru"));
    // Повтор с ключом и другой солью — те же значения во всех ячейках правил.
    let p2 = Profile { settings: settings("другая"), ..p.clone() };
    anonymize(&input, &o2, Some(&key), None, &p2);
    let (g1, g2) = (read_book(&o1), read_book(&o2));
    for (sh, r, c) in ruled_cells(&p, &g1) {
        assert_eq!(g1[&sh].get(r, c).map(|x| &x.text), g2[&sh].get(r, c).map(|x| &x.text), "{sh} {r}:{c}");
    }
}
