//! Доработки 0.6.0 по отчёту аудитора 0.5: фейки адресов и ФИО без ID. Без ID фейк — единственное, что
//! различает людей и адреса (сводные, ВПР): генератор делал 10 000 (адрес) и 1000 (ФИО) случайных попыток,
//! а затем отдавал последний вариант, даже занятый другим значением или равный реальному. Реальные значения
//! из ключа прошлых запусков он не обходил вовсе, «Имя Отчество» сверх 2400 сочетаний перебирал заново
//! для каждого значения (минуты), а ФИО, различающиеся только регистром, считал разными фейками.
//! Каждый тест падал до исправления.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use finboard_anon::fake::{norm_text, Faker, KeyEntry};
use finboard_anon::profile::{Kind, Rule, Settings};

fn settings(salt: &str) -> Settings {
    Settings { salt: salt.into(), ..Settings::default() }
}

/// Сборка с оптимизацией — пять секунд на каждые 50 000 значений (до исправления — минуты); отладочная —
/// с запасом.
fn limit(values: usize) -> Duration {
    let per = if cfg!(debug_assertions) { 300.0 } else { 5.0 };
    Duration::from_secs_f64(per * (values as f64 / 50_000.0).max(1.0))
}

const CITIES: &[&str] = &[
    "Москва", "Санкт-Петербург", "Новосибирск", "Екатеринбург", "Казань", "Нижний Новгород",
    "Челябинск", "Самара", "Омск", "Ростов-на-Дону", "Уфа", "Красноярск", "Воронеж", "Пермь",
    "Волгоград", "Краснодар", "Саратов", "Тюмень", "Тольятти", "Ижевск", "Барнаул", "Ульяновск",
    "Иркутск", "Хабаровск", "Ярославль", "Владивосток", "Томск", "Оренбург", "Кемерово", "Рязань",
];

const STREETS: &[&str] = &[
    "Ленина", "Мира", "Советская", "Садовая", "Лесная", "Школьная", "Центральная", "Молодёжная",
    "Набережная", "Заводская", "Полевая", "Строителей", "Гагарина", "Пушкина", "Кирова",
    "Октябрьская", "Первомайская", "Комсомольская", "Победы", "Солнечная", "Северная", "Южная",
    "Зелёная", "Новая", "Луговая", "Рабочая", "Московская", "Парковая", "Береговая", "Цветочная",
];

const MALE: &[&str] = &[
    "Александр", "Алексей", "Анатолий", "Андрей", "Антон", "Аркадий", "Арсений", "Артём", "Борис",
    "Вадим", "Валентин", "Валерий", "Василий", "Виктор", "Виталий", "Владимир", "Владислав",
    "Всеволод", "Вячеслав", "Геннадий", "Георгий", "Глеб", "Григорий", "Даниил", "Денис",
    "Дмитрий", "Евгений", "Егор", "Захар", "Иван", "Игорь", "Илья", "Кирилл", "Константин", "Лев",
    "Леонид", "Максим", "Марк", "Матвей", "Михаил", "Никита", "Николай", "Олег", "Павел", "Пётр",
    "Роман", "Ростислав", "Руслан", "Святослав", "Семён", "Сергей", "Станислав", "Степан",
    "Тимофей", "Тимур", "Филипп", "Фёдор", "Эдуард", "Юрий", "Ярослав",
];

const FEMALE: &[&str] = &[
    "Александра", "Алёна", "Алина", "Алла", "Анастасия", "Анна", "Антонина", "Арина", "Валентина",
    "Валерия", "Варвара", "Василиса", "Вера", "Вероника", "Виктория", "Галина", "Дарья", "Диана",
    "Ева", "Евгения", "Екатерина", "Елена", "Елизавета", "Жанна", "Злата", "Зоя", "Инна", "Ирина",
    "Карина", "Кира", "Ксения", "Лариса", "Лидия", "Лилия", "Любовь", "Людмила", "Маргарита",
    "Марина", "Мария", "Милана", "Надежда", "Наталья", "Нелли", "Нина", "Оксана", "Олеся", "Ольга",
    "Полина", "Раиса", "Регина", "Светлана", "Снежана", "София", "Таисия", "Тамара", "Татьяна",
    "Ульяна", "Эльвира", "Юлия", "Яна",
];

/// Те же основы отчеств, что у генератора: настоящие ФИО попадают прямо в пространство фейков.
const PATR: &[&str] = &[
    "Александров", "Алексеев", "Анатольев", "Андреев", "Антонов", "Аркадьев", "Борисов", "Вадимов",
    "Валерьев", "Васильев", "Викторов", "Витальев", "Владимиров", "Вячеславов", "Геннадьев",
    "Георгиев", "Григорьев", "Денисов", "Дмитриев", "Евгеньев", "Егоров", "Иванов", "Игорев",
    "Кириллов", "Константинов", "Леонидов", "Максимов", "Михайлов", "Николаев", "Олегов",
    "Павлов", "Петров", "Романов", "Сергеев", "Станиславов", "Степанов", "Тимуров", "Фёдоров",
    "Эдуардов", "Юрьев",
];

/// Шестьдесят фамилий из словаря генератора.
const SUR: &[&str] = &[
    "Абрамов", "Авдеев", "Агафонов", "Александров", "Алексеев", "Андреев", "Анисимов", "Антонов",
    "Баранов", "Белов", "Беляев", "Бобров", "Богданов", "Борисов", "Быков", "Васильев", "Власов",
    "Волков", "Воронов", "Гаврилов", "Голубев", "Григорьев", "Гусев", "Давыдов", "Данилов",
    "Денисов", "Дмитриев", "Егоров", "Ершов", "Жуков", "Захаров", "Зуев", "Ильин", "Исаев",
    "Казаков", "Карпов", "Киселёв", "Козлов", "Комаров", "Королёв", "Котов", "Крылов", "Кузнецов",
    "Лебедев", "Макаров", "Медведев", "Морозов", "Никитин", "Новиков", "Орлов", "Павлов", "Петров",
    "Попов", "Романов", "Смирнов", "Соколов", "Степанов", "Тарасов", "Фролов", "Яковлев",
];

const MORE_MALE: &[&str] = &[
    "Ахмед", "Рустам", "Ильдар", "Ринат", "Марат", "Азат", "Тагир", "Фарид", "Альберт", "Артур", "Давид",
    "Арсен", "Эрик", "Богдан", "Платон", "Мирон", "Демид", "Айдар", "Ислам", "Тихон", "Прохор", "Назар",
];

const LETTERS: &[char] = &[
    'А', 'Б', 'В', 'Г', 'Д', 'Е', 'Ж', 'З', 'И', 'К', 'Л', 'М', 'Н', 'О', 'П', 'Р', 'С', 'Т', 'У', 'Ф', 'Х', 'Э', 'Ю', 'Я',
];

/// `n` различных значений из списка в перемешанном (но неизменном) порядке: шаг 7919 взаимно прост с длиной
/// списка, если длина не кратна 7919.
fn spread(all: &[String], from: usize, n: usize) -> Vec<String> {
    assert!(from + n <= all.len(), "{from}+{n} > {}", all.len());
    (from..from + n).map(|i| all[i * 7919 % all.len()].clone()).collect()
}

/// Адреса в том же виде, что у фейков («г. Москва, ул. Ленина, д. 5»), плотно: настоящие адреса занимают
/// почти половину вариантов первой очереди генератора. Часть — с квартирой и с индексом.
fn addresses(part: usize, parts: usize) -> Vec<String> {
    let mut plain = Vec::new();
    for c in CITIES {
        for s in STREETS {
            for h in 1..=110 {
                plain.push(format!("г. {c}, ул. {s}, д. {h}"));
            }
        }
    }
    let mut flats = Vec::new();
    for c in CITIES.iter().take(4) {
        for s in STREETS {
            for h in 1..=10 {
                for k in 1..=10 {
                    flats.push(format!("г. {c}, ул. {s}, д. {h}, кв. {k}"));
                }
            }
        }
    }
    let indexed: Vec<String> = (0..20_000).map(|i| format!("{}, г. {}, ул. {}, д. {}", 101_000 + i, CITIES[i % 30], STREETS[i / 30 % 30], 1 + i % 7)).collect();
    let (np, nf, ni) = (40_000 / parts, 6_000 / parts, 4_000 / parts);
    let mut out = spread(&plain, part * np, np);
    out.extend(spread(&flats, part * nf, nf));
    out.extend(spread(&indexed, part * ni, ni));
    out
}

/// ФИО без ID: полные мужские и женские из словарей генератора, «Имя Отчество» (без фамилии — у всех одна
/// «семья», 2400 сочетаний на пол), фамилия с инициалами и ФИО с номером (их генератор не разбирает).
fn people(part: usize, parts: usize) -> Vec<String> {
    let mut male = Vec::new();
    let mut female = Vec::new();
    for s in SUR {
        for f in MALE {
            for p in PATR {
                male.push(format!("{s} {f} {p}ич"));
            }
        }
        for f in FEMALE {
            for p in PATR {
                female.push(format!("{s}а {f} {p}на"));
            }
        }
    }
    // Больше сотни отчеств: из словаря генератора, от мужских имён и от имён, которых в словаре нет.
    let stems: Vec<String> = PATR.iter().map(|p| p.to_string()).chain(MALE.iter().chain(MORE_MALE).map(|m| format!("{m}ов"))).collect();
    let mut seen = HashSet::new();
    let stems: Vec<String> = stems.into_iter().filter(|s| seen.insert(norm_text(s))).collect();
    let patr_m: Vec<String> = stems.iter().map(|p| format!("{p}ич")).collect();
    let patr_f: Vec<String> = stems.iter().map(|p| format!("{p}на")).collect();
    let io_m: Vec<String> = MALE.iter().flat_map(|f| patr_m.iter().map(move |p| format!("{f} {p}"))).collect();
    let io_f: Vec<String> = FEMALE.iter().flat_map(|f| patr_f.iter().map(move |p| format!("{f} {p}"))).collect();
    let mut short = Vec::new();
    for s in SUR {
        for a in LETTERS {
            for b in LETTERS {
                short.push(format!("{s} {a}.{b}."));
                short.push(format!("{s}а {a}.{b}."));
            }
        }
    }
    let numbered: Vec<String> = (0..20_000).map(|i| format!("{} {} {}ич {}", SUR[i % 60], MALE[i / 60 % 60], PATR[i % 40], 1 + i / 2400)).collect();
    let (nm, nf, ns, nn) = (20_000 / parts, 10_000 / parts, 5_000 / parts, 5_000 / parts);
    let (iom, iof) = (6_000 / parts, 4_000 / parts);
    let mut out = spread(&male, part * nm, nm);
    out.extend(spread(&female, part * nf, nf));
    // «Имя Отчество» — по порядку: все шестьдесят отчеств одного имени подряд.
    out.extend(io_m[part * iom..(part + 1) * iom].iter().cloned());
    out.extend(io_f[part * iof..(part + 1) * iof].iter().cloned());
    out.extend(spread(&short, part * ns, ns));
    out.extend(spread(&numbered, part * nn, nn));
    out
}

fn rule(kind: Kind, link: &str) -> Rule {
    let mut r = Rule::new("Л", 0, 2, None, kind);
    r.link = link.into();
    r.with_id = false;
    r
}

/// Фейки значений одним генератором (как при обезличивании: сначала все значения прогона, затем фейки).
fn run(f: &mut Faker, rules: &[Rule], reals: &[String]) -> Vec<String> {
    if rules.iter().any(|r| r.kind == Kind::FullName) {
        f.prepare_people(reals.iter().map(String::as_str));
    }
    f.note_reals(reals.iter().map(String::as_str));
    let t = Instant::now();
    let out: Vec<String> = rules.iter().flat_map(|r| reals.iter().map(|x| f.fake(r, x, false)).collect::<Vec<_>>()).collect();
    let took = t.elapsed();
    assert!(took < limit(out.len()), "{} значений — {took:?}", out.len());
    out
}

/// Все фейки разные (без учёта регистра и пробелов — как сравнивает ВПР), ни один не равен реальному.
fn check(what: &str, reals: &[String], fakes: &[String], also_real: &HashSet<String>, also_taken: &HashSet<String>) {
    let real: HashSet<String> = reals.iter().map(|x| norm_text(x)).chain(also_real.iter().cloned()).collect();
    let mut seen: HashMap<String, usize> = HashMap::new();
    let n = reals.len();
    for (i, y) in fakes.iter().enumerate() {
        let k = norm_text(y);
        assert!(!real.contains(&k), "{what}: «{}» → «{y}» — фейк равен реальному значению", reals[i % n]);
        assert!(!also_taken.contains(&k), "{what}: «{}» → «{y}» — фейк из ключа достался другому", reals[i % n]);
        if let Some(j) = seen.insert(k, i) {
            panic!("{what}: «{}» и «{}» получили один фейк «{y}»", reals[j % n], reals[i % n]);
        }
    }
}

fn distinct(v: &[String]) {
    let set: HashSet<String> = v.iter().map(|x| norm_text(x)).collect();
    assert_eq!(set.len(), v.len(), "тестовые значения должны быть разными");
}

/// 50 000 разных адресов без ID — в общей связи и в связи «контрагент» (у связи свои фейки, занятые общие):
/// все 100 000 фейков разные, ни один не равен настоящему адресу, быстро; второй прогон — те же фейки.
#[test]
fn u01_addresses_without_id_unique() {
    let reals = addresses(0, 1);
    assert_eq!(reals.len(), 50_000);
    distinct(&reals);
    let rules = [rule(Kind::Address, ""), rule(Kind::Address, "контрагент")];
    let a = run(&mut Faker::new(&settings("адреса")), &rules, &reals);
    check("адреса", &reals, &a, &HashSet::new(), &HashSet::new());
    let b = run(&mut Faker::new(&settings("адреса")), &rules, &reals);
    assert!(a == b, "второй прогон дал другие фейки");
}

/// 50 000 разных ФИО без ID, в двух связях: все фейки разные, ни один не равен настоящему ФИО, быстро,
/// второй прогон — те же фейки.
#[test]
fn u02_people_without_id_unique() {
    let reals = people(0, 1);
    assert_eq!(reals.len(), 50_000);
    distinct(&reals);
    let rules = [rule(Kind::FullName, ""), rule(Kind::FullName, "контрагент")];
    let a = run(&mut Faker::new(&settings("люди")), &rules, &reals);
    check("ФИО", &reals, &a, &HashSet::new(), &HashSet::new());
    let b = run(&mut Faker::new(&settings("люди")), &rules, &reals);
    assert!(a == b, "второй прогон дал другие фейки");
    // Номер-различитель тёзок — небольшой («Иван Петрович 2»), а не «Иван Петрович 2401» после пустого
    // перебора всех сочетаний.
    for y in &a {
        if let Some(n) = y.rsplit(' ').next().and_then(|t| t.parse::<usize>().ok()) {
            assert!(n < 100, "«{y}»: номер {n} — перебор вариантов впустую");
        }
    }
}

/// С ключом прошлого запуска: новые значения не получают ни фейков из ключа, ни настоящих значений из ключа
/// (они тоже чьи-то данные), между собой все разные; повтор с тем же ключом — те же фейки.
fn with_key(kind: Kind, gen: fn(usize, usize) -> Vec<String>) {
    let (old, new) = (gen(0, 2), gen(1, 2));
    let all: Vec<String> = old.iter().chain(&new).cloned().collect();
    distinct(&all);
    let rules = [rule(kind, ""), rule(kind, "контрагент")];
    let mut first = Faker::new(&settings("ключ"));
    let old_fakes = run(&mut first, &rules, &old);
    check("прошлый запуск", &old, &old_fakes, &HashSet::new(), &HashSet::new());
    let key: Vec<KeyEntry> = first.entries.clone();
    let key_fakes: HashSet<String> = key.iter().map(|e| norm_text(&e.fake)).collect();
    let old_reals: HashSet<String> = old.iter().map(|x| norm_text(x)).collect();
    let go = || {
        let mut f = Faker::new(&settings("ключ"));
        f.load_entries(key.clone());
        run(&mut f, &rules, &new)
    };
    let a = go();
    check("с ключом", &new, &a, &old_reals, &key_fakes);
    assert!(a == go(), "второй прогон с тем же ключом дал другие фейки");
}

#[test]
fn u03_addresses_with_key() {
    with_key(Kind::Address, addresses);
}

#[test]
fn u04_people_with_key() {
    with_key(Kind::FullName, people);
}

/// ФИО одного человека в разном регистре — один фейк; разные люди не получают фейков, которые различаются
/// только регистром («ПЕТРОВ ОЛЕГ ЮРЬЕВИЧ» и «Петров Олег Юрьевич» в ВПР — одно и то же).
#[test]
fn u05_people_case_insensitive() {
    let mut reals = people(0, 1);
    reals.truncate(30_000);
    let upper: Vec<String> = reals.iter().enumerate().map(|(i, x)| if i % 2 == 0 { x.to_uppercase() } else { x.clone() }).collect();
    let rules = [rule(Kind::FullName, "")];
    let mut f = Faker::new(&settings("регистр"));
    let a = run(&mut f, &rules, &upper);
    check("ФИО в разном регистре", &upper, &a, &HashSet::new(), &HashSet::new());
    // Тот же человек другим регистром — тот же фейк (с точностью до регистра).
    for (x, y) in reals.iter().zip(&a).step_by(2).take(2000) {
        let again = f.fake(&rules[0], x, false);
        assert_eq!(norm_text(&again), norm_text(y), "«{x}»: другой регистр — другой фейк");
    }
}
