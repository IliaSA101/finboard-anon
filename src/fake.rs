//! Генератор правдоподобных фейков с ID для прослеживаемости.
//!
//! Правила согласованности:
//! * одно и то же исходное значение под одним типом всегда даёт один и тот же фейк;
//! * ID выдаётся по нормализованному значению: «+7 916 123-45-67» и «89161234567» — один ID;
//! * части ФИО заменяются через общие словари: фамилия «Иванов» в колонке «Фамилия»
//!   и в полном ФИО превратится в одну и ту же фейковую фамилию, с учётом пола;
//! * «Иванов И.И.» привязывается к «Иванов Иван Иванович», если такой человек в данных один.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use crate::names::*;
use crate::profile::{parse_kind_key, Kind, Rule, Settings};

// ---------------------------------------------------------------- RNG

/// Детерминированный генератор: FNV-1a от частей ключа + splitmix64.
struct Rng(u64);

impl Rng {
    fn new(parts: &[&str]) -> Rng {
        let mut h: u64 = 0xcbf29ce484222325;
        for p in parts {
            for b in p.bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(0x100000001b3);
            }
            h ^= 0xff;
            h = h.wrapping_mul(0x100000001b3);
        }
        Rng(h)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    fn pick<'a>(&mut self, list: &[&'a str]) -> &'a str {
        list[self.below(list.len())]
    }

    fn digits(&mut self, n: usize, first_nonzero: bool) -> String {
        (0..n)
            .map(|i| {
                let d = if i == 0 && first_nonzero { 1 + self.below(9) } else { self.below(10) };
                (b'0' + d as u8) as char
            })
            .collect()
    }
}

// ---------------------------------------------------------------- текстовые утилиты

/// Нижний регистр, ё→е, без лишних пробелов — для сравнения значений.
pub fn norm_text(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
        .replace('ё', "е")
}

fn norm_word(s: &str) -> String {
    s.to_lowercase().replace('ё', "е")
}

fn digits_of(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_digit()).collect()
}

/// Короткий код вроде «Дог1», «Налог10», «Счет 7»: до 4 цифр — не номер счёта, а условное обозначение.
/// Номер ID на месте его цифр совпадал бы с самим кодом («Дог1» → «Дог1») или с соседним («Дог2» → «Дог1»).
fn short_code(d: &str) -> bool {
    (1..=4).contains(&d.len())
}

/// Цифры первой группы заменяются новыми, цифры остальных групп убираются: «Дог1» → «Дог900001».
fn replace_digit_run(orig: &str, new_digits: &str) -> String {
    let mut out = String::with_capacity(orig.len() + new_digits.len());
    let (mut done, mut in_run) = (false, false);
    for c in orig.chars() {
        if c.is_ascii_digit() {
            if !done && !in_run {
                out.push_str(new_digits);
            }
            in_run = true;
        } else {
            if in_run {
                done = true;
            }
            in_run = false;
            out.push(c);
        }
    }
    out
}

fn title_case(s: &str) -> String {
    let lower = s.to_lowercase();
    let mut out = String::new();
    let mut up = true;
    for c in lower.chars() {
        if up && c.is_alphabetic() {
            out.extend(c.to_uppercase());
            up = false;
        } else {
            out.push(c);
        }
        if c == '-' {
            up = true;
        }
    }
    out
}

/// Переносит регистр оригинала (ВСЕ ЗАГЛАВНЫЕ / все строчные) на фейк.
fn apply_case(orig: &str, fake: &str) -> String {
    let letters: Vec<char> = orig.chars().filter(|c| c.is_alphabetic()).collect();
    if letters.len() > 1 && letters.iter().all(|c| c.is_uppercase()) {
        fake.to_uppercase()
    } else if !letters.is_empty() && letters.iter().all(|c| c.is_lowercase()) {
        fake.to_lowercase()
    } else {
        fake.to_string()
    }
}

/// Подставляет новые цифры на места цифр оригинала, сохраняя скобки, дефисы, пробелы.
fn fill_digits(orig: &str, new_digits: &str) -> String {
    let mut it = new_digits.chars();
    orig.chars()
        .map(|c| if c.is_ascii_digit() { it.next().unwrap_or('0') } else { c })
        .collect()
}

/// ID с ведущими нулями до ширины `width` (если ID длиннее — берутся последние цифры).
fn pad_id(id: u32, width: usize) -> String {
    let s = id.to_string();
    if s.len() >= width {
        s[s.len() - width..].to_string()
    } else {
        format!("{}{}", "0".repeat(width - s.len()), s)
    }
}

fn translit(s: &str) -> String {
    let mut out = String::new();
    for c in s.to_lowercase().chars() {
        let t = match c {
            'а' => "a", 'б' => "b", 'в' => "v", 'г' => "g", 'д' => "d", 'е' | 'ё' | 'э' => "e",
            'ж' => "zh", 'з' => "z", 'и' => "i", 'й' | 'ы' => "y", 'к' => "k", 'л' => "l",
            'м' => "m", 'н' => "n", 'о' => "o", 'п' => "p", 'р' => "r", 'с' => "s", 'т' => "t",
            'у' => "u", 'ф' => "f", 'х' => "kh", 'ц' => "ts", 'ч' => "ch", 'ш' => "sh",
            'щ' => "shch", 'ъ' | 'ь' => "", 'ю' => "yu", 'я' => "ya",
            c if c.is_ascii_alphanumeric() => {
                out.push(c);
                continue;
            }
            _ => "",
        };
        out.push_str(t);
    }
    out
}

fn name_set(lists: &[&[&str]]) -> HashSet<String> {
    lists.iter().flat_map(|l| l.iter()).map(|n| norm_word(n)).collect()
}

fn male_names() -> &'static HashSet<String> {
    static S: OnceLock<HashSet<String>> = OnceLock::new();
    S.get_or_init(|| name_set(&[MALE, DETECT_MALE]))
}

fn female_names() -> &'static HashSet<String> {
    static S: OnceLock<HashSet<String>> = OnceLock::new();
    S.get_or_init(|| name_set(&[FEMALE, DETECT_FEMALE]))
}

fn is_first_name(lower: &str) -> bool {
    male_names().contains(lower) || female_names().contains(lower)
}

fn is_female_first(lower: &str) -> bool {
    if female_names().contains(lower) {
        return true;
    }
    if male_names().contains(lower) {
        return false;
    }
    lower.ends_with('а') || lower.ends_with('я')
}

fn is_patronymic(lower: &str) -> bool {
    lower.chars().count() >= 5
        && (lower.ends_with("вич")
            || lower.ends_with("вна")
            || lower.ends_with("ична")
            || lower.ends_with("ич"))
}

fn is_female_patronymic(lower: &str) -> bool {
    lower.ends_with("вна") || lower.ends_with("чна") || lower.ends_with("кызы")
}

fn is_female_surname(lower: &str) -> bool {
    ["ова", "ева", "ёва", "ина", "ына", "ская", "цкая"].iter().any(|e| lower.ends_with(e))
}

/// Женская фамилия → мужская основа (Иванова → Иванов, Покровская → Покровский).
fn masculinize(s: &str, female: bool) -> String {
    if !female {
        return s.to_string();
    }
    for (f, m) in [("ская", "ский"), ("цкая", "цкий")] {
        if let Some(stem) = s.strip_suffix(f) {
            return format!("{stem}{m}");
        }
    }
    if ["ова", "ева", "ёва", "ина", "ына"].iter().any(|e| s.ends_with(e)) {
        let mut v: Vec<char> = s.chars().collect();
        v.pop();
        return v.into_iter().collect();
    }
    s.to_string()
}

fn feminize(s: &str) -> String {
    for (m, f) in [("ский", "ская"), ("цкий", "цкая")] {
        if let Some(stem) = s.strip_suffix(m) {
            return format!("{stem}{f}");
        }
    }
    if ["ов", "ев", "ёв", "ин", "ын"].iter().any(|e| s.ends_with(e)) {
        return format!("{s}а");
    }
    s.to_string()
}

/// Ключ отчества без родовой части: Иванович / Ивановна → иванов.
fn patr_key(lower: &str) -> String {
    if lower.ends_with("вич") || lower.ends_with("вна") {
        let mut v: Vec<char> = lower.chars().collect();
        v.truncate(v.len() - 2);
        return v.into_iter().collect();
    }
    lower.to_string()
}

/// Название организации на части: (до названия, название, после).
/// «ООО «Ромашка»» → («ООО «», «Ромашка», «»»); «Общество с ограниченной ответственностью "Ромашка"» — так же;
/// «Ромашка, ООО» → («», «Ромашка», «, ООО»); «ИП Козлов О.П.» → («ИП », «Козлов О.П.», «»).
/// Одна организация в разных написаниях даёт одно название — по нему и выбирается фейк.
pub fn split_company(s: &str) -> (String, String, String) {
    let t = s.trim();
    for (a, b) in [('«', '»'), ('"', '"'), ('“', '”'), ('„', '“')] {
        if let (Some(i), Some(j)) = (t.find(a), t.rfind(b)) {
            let from = i + a.len_utf8();
            if j > i && !t[from..j].trim().is_empty() {
                return (t[..from].to_string(), t[from..j].to_string(), t[j..].to_string());
            }
        }
    }
    let is_form = |w: &str| LEGAL_FORMS.contains(&w.trim_matches(|c: char| !c.is_alphabetic()).to_uppercase().as_str());
    let toks: Vec<&str> = t.split_whitespace().collect();
    if toks.len() > 1 && is_form(toks[0]) {
        let rest = t[toks[0].len()..].trim_start();
        let pre = &t[..t.len() - rest.len()];
        return (pre.to_string(), rest.to_string(), String::new());
    }
    if toks.len() > 1 && is_form(toks[toks.len() - 1]) {
        let last = toks[toks.len() - 1];
        let body = t[..t.len() - last.len()].trim_end().trim_end_matches(',').trim_end();
        return (String::new(), body.to_string(), t[body.len()..].to_string());
    }
    (String::new(), t.to_string(), String::new())
}

/// Слова полной организационно-правовой формы («Общество с ограниченной ответственностью», «Акционерное общество»…).
const FORM_WORDS: &[&str] = &[
    "общество", "с", "ограниченной", "ответственностью", "акционерное", "публичное", "непубличное", "закрытое", "открытое",
    "индивидуальный", "предприниматель", "государственное", "муниципальное", "федеральное", "унитарное", "предприятие",
    "некоммерческая", "автономная", "организация", "товарищество",
];

/// Снаружи названия в результате остаются только форма (ООО, АО, ИП, «Общество с ограниченной ответственностью»)
/// и знаки: «(М) ООО «» → «ООО «», «» (Москва)» → «»». Прочие слова и номера рядом с названием — тоже данные
/// заказчика (пометки, города, филиалы): они не переносятся в фейк.
fn form_only(part: &str) -> String {
    let form = |w: &str| {
        let letters = w.trim_matches(|c: char| !c.is_alphabetic());
        LEGAL_FORMS.contains(&letters.to_uppercase().as_str()) || FORM_WORDS.contains(&letters.to_lowercase().as_str())
    };
    let kept: Vec<&str> = part.split_whitespace().filter(|w| !w.chars().any(char::is_alphanumeric) || form(w)).collect();
    if kept.is_empty() {
        return String::new();
    }
    let lead = if part.starts_with(char::is_whitespace) { " " } else { "" };
    let tail = if part.ends_with(char::is_whitespace) { " " } else { "" };
    format!("{lead}{}{tail}", kept.join(" "))
}

/// Ключ организации: название без формы и кавычек, без регистра.
pub fn company_key(s: &str) -> String {
    norm_text(&split_company(strip_id_suffix(s)).1)
}

/// Индивидуальный предприниматель: «ИП Козлов Олег Петрович» — это человек.
fn is_ip(pre: &str, post: &str) -> bool {
    post.is_empty() && pre.trim().trim_matches(|c: char| !c.is_alphabetic()).to_uppercase() == "ИП"
}

fn is_initials(t: &str) -> bool {
    let letters = t.chars().filter(|c| c.is_alphabetic()).count();
    t.contains('.')
        && (1..=3).contains(&letters)
        && t.chars().all(|c| c.is_alphabetic() || c == '.')
        && t.chars().count() <= 6
}

// ---------------------------------------------------------------- разбор ФИО

#[derive(Clone, Copy, PartialEq, Debug)]
enum Role {
    Surname,
    First,
    Patr,
    Initials,
    Keep,
}

#[derive(Clone, Debug)]
struct Person {
    toks: Vec<String>,
    roles: Vec<Role>,
    female: bool,
}

impl Person {
    fn tok(&self, role: Role) -> Option<&str> {
        self.roles.iter().position(|r| *r == role).map(|i| self.toks[i].as_str())
    }

    fn initial_letters(&self) -> Vec<char> {
        self.roles
            .iter()
            .zip(&self.toks)
            .filter(|(r, _)| **r == Role::Initials)
            .flat_map(|(_, t)| t.chars().filter(|c| c.is_alphabetic()))
            .flat_map(|c| c.to_uppercase())
            .collect()
    }

    fn is_initials_form(&self) -> bool {
        self.tok(Role::First).is_none() && self.tok(Role::Surname).is_some() && self.initial_letters().len() >= 2
    }

    /// Ключ «фамилия + две первые буквы имени/отчества» для связи сокращённой и полной формы.
    fn link_key(&self) -> Option<(String, char, char)> {
        let sur = masculinize(&norm_word(self.tok(Role::Surname)?), self.female);
        if self.is_initials_form() {
            let l = self.initial_letters();
            return Some((sur, l[0], l[1]));
        }
        let f = self.tok(Role::First)?.chars().next()?.to_uppercase().next()?;
        let p = self.tok(Role::Patr)?.chars().next()?.to_uppercase().next()?;
        Some((sur, f, p))
    }
}

/// Убирает приписку вида «(ID 12)» в конце — она остаётся от прошлого обезличивания.
pub fn strip_id_suffix(s: &str) -> &str {
    let t = s.trim_end();
    if let Some(body) = t.strip_suffix(')') {
        if let Some(open) = body.rfind('(') {
            let inner = body[open + 1..].trim();
            let digits = ["ID", "Id", "iD", "id"].iter().find_map(|p| inner.strip_prefix(p)).map(str::trim);
            if digits.is_some_and(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit())) {
                return t[..open].trim_end();
            }
        }
    }
    s
}

fn parse_person(s: &str) -> Option<Person> {
    let toks: Vec<String> = s.split_whitespace().map(str::to_string).collect();
    if toks.is_empty() {
        return None;
    }
    let mut roles = vec![Role::Keep; toks.len()];
    let mut words = Vec::new();
    let mut has_initials = false;
    let mut in_paren = false;
    for (i, t) in toks.iter().enumerate() {
        let l = norm_word(t);
        // Пометки в скобках («(стажёр)», «(ГИП)») остаются как есть.
        if in_paren || t.starts_with('(') {
            in_paren = !t.ends_with(')');
            continue;
        }
        if t.chars().any(|c| c.is_ascii_digit()) {
            return None;
        }
        if is_initials(t) {
            roles[i] = Role::Initials;
            has_initials = true;
        } else if l == "оглы" || l == "кызы" {
            // остаётся как есть
        } else if t.chars().any(|c| c.is_alphabetic())
            && t.chars().all(|c| c.is_alphabetic() || c == '-' || c == '\'')
        {
            words.push(i);
        } else {
            return None;
        }
    }
    let lw: Vec<String> = words.iter().map(|&i| norm_word(&toks[i])).collect();
    let set = |roles: &mut Vec<Role>, k: usize, r: Role| roles[words[k]] = r;
    match words.len() {
        0 => return None,
        1 => {
            let r = if has_initials {
                Role::Surname
            } else if is_first_name(&lw[0]) {
                Role::First
            } else if is_patronymic(&lw[0]) {
                Role::Patr
            } else {
                Role::Surname
            };
            set(&mut roles, 0, r);
        }
        2 => {
            if is_patronymic(&lw[1]) && !has_initials {
                set(&mut roles, 0, Role::First);
                set(&mut roles, 1, Role::Patr);
            } else if is_first_name(&lw[0]) && !is_first_name(&lw[1]) {
                set(&mut roles, 0, Role::First);
                set(&mut roles, 1, Role::Surname);
            } else {
                set(&mut roles, 0, Role::Surname);
                set(&mut roles, 1, Role::First);
            }
        }
        _ => {
            if is_patronymic(&lw[1]) && !is_patronymic(&lw[2]) {
                // Иван Иванович Иванов
                set(&mut roles, 0, Role::First);
                set(&mut roles, 1, Role::Patr);
                set(&mut roles, 2, Role::Surname);
            } else {
                set(&mut roles, 0, Role::Surname);
                set(&mut roles, 1, Role::First);
                set(&mut roles, 2, Role::Patr);
            }
            for k in 3..words.len() {
                set(&mut roles, k, Role::Surname);
            }
        }
    }
    let mut p = Person { toks, roles, female: false };
    p.female = if let Some(t) = p.tok(Role::Patr) {
        is_female_patronymic(&norm_word(t))
    } else if let Some(t) = p.tok(Role::First) {
        is_female_first(&norm_word(t))
    } else if let Some(t) = p.tok(Role::Surname) {
        is_female_surname(&norm_word(t))
    } else {
        false
    };
    Some(p)
}

// ---------------------------------------------------------------- даты

pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

pub fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn pad_num(v: i64, width: usize) -> String {
    format!("{:0width$}", v, width = width)
}

/// Сдвигает дату в тексте «дд.мм.гггг», «дд/мм/гггг» или «гггг-мм-дд[...]».
/// Номер телефона без кода страны: «79161234567», «89161234567», «9161234567» → «9161234567».
pub fn phone_key(digits: &str) -> String {
    let d = digits_of(digits);
    if d.len() == 11 && (d.starts_with('7') || d.starts_with('8')) {
        d[1..].to_string()
    } else {
        d
    }
}

/// День даты (число дней от 1970-01-01): дата Excel числом или текстом «15.07.1980», «1980-07-15».
fn date_day(s: &str, numeric: bool) -> Option<i64> {
    let t = s.trim();
    if numeric || t.parse::<f64>().is_ok() {
        let v: f64 = t.parse().ok()?;
        return (v >= 1.0).then(|| days_from_civil(1899, 12, 30) + v.trunc() as i64);
    }
    let date = t.split([' ', 'T']).next()?;
    let sep = ['.', '/', '-'].into_iter().find(|c| date.contains(*c))?;
    let parts: Vec<&str> = date.split(sep).collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty() || !p.chars().all(|c| c.is_ascii_digit())) {
        return None;
    }
    let (yi, di) = if parts[0].len() == 4 { (0, 2) } else { (2, 0) };
    let y: i64 = parts[yi].parse().ok()?;
    let y = if parts[yi].len() == 2 { 1900 + y } else { y };
    let (m, d): (i64, i64) = (parts[1].parse().ok()?, parts[di].parse().ok()?);
    ((1..=12).contains(&m) && (1..=31).contains(&d)).then(|| days_from_civil(y, m, d))
}

fn shift_text_date(s: &str, offset: i64) -> Option<String> {
    let (date, rest) = match s.find([' ', 'T']) {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    };
    let sep = if date.contains('.') {
        '.'
    } else if date.contains('/') {
        '/'
    } else if date.contains('-') {
        '-'
    } else {
        return None;
    };
    let parts: Vec<&str> = date.split(sep).collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty() || !p.chars().all(|c| c.is_ascii_digit())) {
        return None;
    }
    let iso = parts[0].len() == 4;
    let (yi, di) = if iso { (0, 2) } else { (2, 0) };
    let y: i64 = parts[yi].parse().ok()?;
    let m: i64 = parts[1].parse().ok()?;
    let d: i64 = parts[di].parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let full_year = if parts[yi].len() == 2 { 1900 + y } else { y };
    let (ny, nm, nd) = civil_from_days(days_from_civil(full_year, m, d) + offset);
    let ys = if parts[yi].len() == 2 { pad_num(ny % 100, 2) } else { pad_num(ny, parts[yi].len()) };
    let ms = pad_num(nm, parts[1].len());
    let ds = pad_num(nd, parts[di].len());
    let date = if iso {
        format!("{ys}{sep}{ms}{sep}{ds}")
    } else {
        format!("{ds}{sep}{ms}{sep}{ys}")
    };
    Some(format!("{date}{rest}"))
}

// ---------------------------------------------------------------- генератор

#[derive(Clone, Debug)]
pub struct KeyEntry {
    pub kind_key: String,
    pub id: Option<u32>,
    pub real: String,
    pub fake: String,
}

pub struct Faker {
    s: Settings,
    ids: HashMap<String, HashMap<String, u32>>,
    next_id: HashMap<String, u32>,
    last_id: Option<u32>,
    pub entries: Vec<KeyEntry>,
    cache: HashMap<(String, String), usize>,
    surname: HashMap<String, String>,
    used_surname: HashSet<String>,
    first: [HashMap<String, String>; 2],
    used_first: [HashSet<String>; 2],
    patr: HashMap<String, usize>,
    used_patr: HashSet<usize>,
    used_email: HashSet<String>,
    used_company: HashSet<String>,
    /// Название организации (ключ) → фейковое название: «ООО «Ромашка»» и «Ромашка» — одна организация.
    company: HashMap<String, String>,
    /// Название как написано в данных → фейковое: для замены внутри текста («Оплата ООО "Ромашка"»).
    /// Название организации как в данных → фейковое название (без формы) и ID, если правило с ID.
    company_text: HashMap<String, (String, Option<u32>)>,
    /// Цифры ИНН, СНИЛС, паспорта, счёта (от 9 цифр) → фейковые цифры: один номер в любом виде
    /// («7707083893», «77 07 083893», «ИНН: 7707083893») и под любым из этих типов — один фейк.
    digits: HashMap<String, String>,
    /// Телефон по 10 цифрам номера (без 8/+7) → фейковые 10 цифр: «8 916…», «+7 (916)…», число 7916… — один фейк.
    phones: HashMap<String, String>,
    /// Email без регистра → фейк.
    emails: HashMap<String, String>,
    /// Адрес без регистра и лишних пробелов → фейк (без ID).
    addresses: HashMap<String, String>,
    /// День даты → сдвиг в днях: дата числом Excel и та же дата текстом «15.07.1980» сдвигаются одинаково.
    date_shift: HashMap<i64, i64>,
    /// Фейки с ID из загруженного ключа: повторный прогон по уже обезличенному файлу их не трогает.
    known_fakes: HashSet<String>,
    /// Фейковое ФИО без ID → чьё оно (нормализованное исходное): разные люди не должны сливаться.
    used_person: HashMap<String, String>,
    /// Строки, которые уже есть в книге (реальные значения, шапки): фейк не должен с ними совпасть —
    /// иначе две колонки умной таблицы назовутся одинаково или фейк совпадёт с другим человеком.
    taken: HashSet<String>,
    people: HashMap<(String, char, char), HashSet<String>>,
    full_by_norm: HashMap<String, String>,
}

impl Faker {
    pub fn new(settings: &Settings) -> Faker {
        Faker {
            s: settings.clone(),
            ids: HashMap::new(),
            next_id: HashMap::new(),
            last_id: None,
            entries: Vec::new(),
            cache: HashMap::new(),
            surname: HashMap::new(),
            used_surname: HashSet::new(),
            first: [HashMap::new(), HashMap::new()],
            used_first: [HashSet::new(), HashSet::new()],
            patr: HashMap::new(),
            used_patr: HashSet::new(),
            used_email: HashSet::new(),
            used_company: HashSet::new(),
            company: HashMap::new(),
            company_text: HashMap::new(),
            digits: HashMap::new(),
            phones: HashMap::new(),
            emails: HashMap::new(),
            addresses: HashMap::new(),
            date_shift: HashMap::new(),
            known_fakes: HashSet::new(),
            used_person: HashMap::new(),
            taken: HashSet::new(),
            people: HashMap::new(),
            full_by_norm: HashMap::new(),
        }
    }

    /// Строки книги, с которыми фейки не должны совпадать.
    pub fn set_taken<'a>(&mut self, strings: impl IntoIterator<Item = &'a str>) {
        for s in strings {
            self.taken.insert(norm_text(s));
            // «ООО «Вектор»» в книге — фейковой организации «Вектор» быть не должно.
            self.taken.insert(company_key(s));
        }
    }

    /// Организации: название как в данных → фейковое название (без формы) и ID.
    pub fn company_names(&self) -> impl Iterator<Item = (&str, &str, Option<u32>)> {
        self.company_text.iter().map(|(a, (b, id))| (a.as_str(), b.as_str(), *id))
    }

    /// Что ставится до и после фейка при добавлении ID: для «{fake} (ID {id})» — ("", " (ID 3)").
    pub fn id_affixes(&self, id: u32) -> (String, String) {
        let fmt = self.s.id_format.replace("{id}", &id.to_string());
        match fmt.split_once("{fake}") {
            Some((pre, post)) => (pre.to_string(), post.to_string()),
            None => (String::new(), String::new()),
        }
    }

    /// Готовый фейк, если значение уже обработано.
    pub fn get(&self, kind_key: &str, original: &str) -> Option<&str> {
        self.cache
            .get(&(kind_key.to_string(), original.to_string()))
            .map(|&i| self.entries[i].fake.as_str())
            .or_else(|| self.known_fakes.get(original).map(String::as_str))
    }

    /// Цифры номеров (ИНН, СНИЛС, паспорт, счёт): реальные → фейковые.
    pub fn digit_pairs(&self) -> impl Iterator<Item = (&str, &str)> {
        self.digits.iter().map(|(a, b)| (a.as_str(), b.as_str()))
    }

    /// Телефоны: 10 цифр номера, реальные → фейковые.
    pub fn phone_pairs(&self) -> impl Iterator<Item = (&str, &str)> {
        self.phones.iter().map(|(a, b)| (a.as_str(), b.as_str()))
    }

    /// Запоминает полные ФИО, чтобы потом связать с ними сокращённые («Иванов И.И.»).
    pub fn prepare_people<'a>(&mut self, originals: impl IntoIterator<Item = &'a str>) {
        for o in originals {
            let o = strip_id_suffix(o);
            if let Some(p) = parse_person(o) {
                if p.is_initials_form() {
                    continue;
                }
                if let Some(k) = p.link_key() {
                    let n = norm_text(o);
                    self.people.entry(k).or_default().insert(n.clone());
                    self.full_by_norm.entry(n).or_insert_with(|| o.to_string());
                }
            }
        }
    }

    pub fn fake(&mut self, rule: &Rule, original: &str, numeric: bool) -> String {
        let key = (rule.kind_key(), original.to_string());
        if let Some(&i) = self.cache.get(&key) {
            return self.entries[i].fake.clone();
        }
        // Уже фейк из ключа (повторный прогон по результату) — оставить как есть.
        if self.known_fakes.contains(original) {
            return original.to_string();
        }
        self.last_id = None;
        let w = rule.with_id_effective();
        let out = match rule.kind {
            Kind::FullName => self.fake_person(original, w),
            Kind::LastName | Kind::FirstName | Kind::MiddleName => self.fake_name_part(rule.kind, original, w),
            Kind::Email => self.map_multi(Kind::Email, original, w),
            Kind::Phone => self.map_multi(Kind::Phone, original, w),
            Kind::Inn | Kind::Snils | Kind::Passport | Kind::Account => self.fake_number(rule.kind, original, w),
            Kind::Address => self.fake_address(original, w),
            Kind::BirthDate => self.fake_date(original, numeric),
            Kind::Company => self.fake_company(original, w),
            Kind::Mask => self.mask(original),
            Kind::Text => {
                let label = rule.label_or_default().to_string();
                let id = self.id_for(&format!("text:{label}"), &norm_text(original));
                format!("{label} {id}")
            }
            Kind::Clear => String::new(),
            // Защищённые области в замены не попадают (collect их пропускает).
            Kind::Keep => original.to_string(),
        };
        self.entries.push(KeyEntry { kind_key: key.0.clone(), id: self.last_id, real: original.to_string(), fake: out.clone() });
        self.cache.insert(key, self.entries.len() - 1);
        out
    }

    fn id_for(&mut self, ns: &str, norm: &str) -> u32 {
        let map = self.ids.entry(ns.to_string()).or_default();
        let id = match map.get(norm) {
            Some(&id) => id,
            None => {
                let next = self.next_id.entry(ns.to_string()).or_insert(1);
                let id = *next;
                *next += 1;
                map.insert(norm.to_string(), id);
                id
            }
        };
        self.last_id = Some(id);
        id
    }

    /// Номер, который получит (или уже получило) значение, — без побочных эффектов `id_for`.
    fn peek_id(&self, ns: &str, norm: &str) -> u32 {
        match self.ids.get(ns).and_then(|m| m.get(norm)) {
            Some(&id) => id,
            None => self.next_id.get(ns).copied().unwrap_or(1),
        }
    }

    fn with_id(&self, fake: &str, id: u32) -> String {
        self.s.id_format.replace("{fake}", fake).replace("{id}", &id.to_string())
    }

    fn strip_id(&self, fake: &str, id: u32) -> String {
        let fmt = self.s.id_format.replace("{id}", &id.to_string());
        match fmt.split_once("{fake}") {
            Some((pre, post)) => fake
                .strip_prefix(pre)
                .and_then(|f| f.strip_suffix(post))
                .unwrap_or(fake)
                .to_string(),
            None => fake.to_string(),
        }
    }

    // ------------------------------------------------ части ФИО

    fn pick_unique(
        list: &[&'static str],
        used: &HashSet<String>,
        avoid: &str,
        r: &mut Rng,
        reject: &dyn Fn(&str) -> bool,
    ) -> Option<&'static str> {
        let ok = |c: &str| {
            let n = norm_word(c);
            !used.contains(&n) && n != avoid && !reject(c)
        };
        for _ in 0..40 {
            let c = r.pick(list);
            if ok(c) {
                return Some(c);
            }
        }
        let start = r.below(list.len());
        (0..list.len()).map(|i| list[(start + i) % list.len()]).find(|c| ok(c))
    }

    fn synth_surname(&self, r: &mut Rng) -> String {
        const C: &[&str] = &["б", "в", "г", "д", "з", "к", "л", "м", "н", "п", "р", "с", "т", "ф", "х", "ч", "ш"];
        const V: &[&str] = &["а", "е", "и", "о", "у"];
        const E: &[&str] = &["ов", "ев", "ин", "ский"];
        loop {
            let s = format!("{}{}{}{}{}{}", r.pick(C), r.pick(V), r.pick(C), r.pick(V), r.pick(C), r.pick(E));
            let s = title_case(&s);
            if !self.used_surname.contains(&norm_word(&s)) && !self.taken.contains(&norm_word(&s)) {
                return s;
            }
        }
    }

    fn map_surname(&mut self, tok: &str, female: bool) -> String {
        let base_key = masculinize(&norm_word(tok), female);
        let base = match self.surname.get(&base_key) {
            Some(f) => f.clone(),
            None => {
                let mut r = Rng::new(&[&self.s.salt, "sur", &base_key]);
                let taken = &self.taken;
                let reject = |c: &str| taken.contains(&norm_word(c)) || taken.contains(&norm_word(&feminize(c)));
                let f = match Self::pick_unique(SURNAMES, &self.used_surname, &base_key, &mut r, &reject) {
                    Some(f) => f.to_string(),
                    None => self.synth_surname(&mut r),
                };
                self.used_surname.insert(norm_word(&f));
                self.surname.insert(base_key, f.clone());
                f
            }
        };
        apply_case(tok, &if female { feminize(&base) } else { base })
    }

    fn map_first(&mut self, tok: &str, female: bool) -> String {
        let key = norm_word(tok);
        let g = female as usize;
        let fake = match self.first[g].get(&key) {
            Some(f) => f.clone(),
            None => {
                let list = if female { FEMALE } else { MALE };
                let mut r = Rng::new(&[&self.s.salt, "first", &key, if female { "f" } else { "m" }]);
                let taken = &self.taken;
                let f = Self::pick_unique(list, &self.used_first[g], &key, &mut r, &|c: &str| taken.contains(&norm_word(c)))
                    .unwrap_or_else(|| r.pick(list))
                    .to_string();
                self.used_first[g].insert(norm_word(&f));
                self.first[g].insert(key, f.clone());
                f
            }
        };
        apply_case(tok, &fake)
    }

    fn map_patr(&mut self, tok: &str, female: bool) -> String {
        let key = patr_key(&norm_word(tok));
        let idx = match self.patr.get(&key) {
            Some(&i) => i,
            None => {
                let mut r = Rng::new(&[&self.s.salt, "patr", &key]);
                let n = PATR_STEMS.len();
                let start = r.below(n);
                let i = (0..n)
                    .map(|k| (start + k) % n)
                    .find(|i| {
                        let st = PATR_STEMS[*i];
                        !self.used_patr.contains(i)
                            && norm_word(st) != key
                            && !self.taken.contains(&norm_word(&format!("{st}ич")))
                            && !self.taken.contains(&norm_word(&format!("{st}на")))
                    })
                    .unwrap_or(start);
                self.used_patr.insert(i);
                self.patr.insert(key, i);
                i
            }
        };
        let out = format!("{}{}", PATR_STEMS[idx], if female { "на" } else { "ич" });
        apply_case(tok, &out)
    }

    fn fake_person(&mut self, original: &str, with_id: bool) -> String {
        let original = strip_id_suffix(original);
        let norm = norm_text(original);
        let Some(p) = parse_person(original) else {
            return self.fallback_person(&norm, with_id);
        };
        let mut id_norm = norm.clone();
        let mut source: Option<Person> = None;
        if p.is_initials_form() {
            if let Some(set) = p.link_key().and_then(|k| self.people.get(&k)) {
                if set.len() == 1 {
                    let full = set.iter().next().unwrap().clone();
                    source = self.full_by_norm.get(&full).and_then(|o| parse_person(o));
                    id_norm = full;
                }
            }
        }
        let female = source.as_ref().map_or(p.female, |s| s.female);
        let initials: Vec<char> = match &source {
            Some(src) => {
                let f = src.tok(Role::First).unwrap_or("").to_string();
                let pt = src.tok(Role::Patr).unwrap_or("").to_string();
                let f = self.map_first(&f, female);
                let pt = self.map_patr(&pt, female);
                [f, pt].iter().filter_map(|s| s.chars().next()).flat_map(|c| c.to_uppercase()).collect()
            }
            None => {
                let mut r = Rng::new(&[&self.s.salt, "init", &norm]);
                let f = r.pick(if female { FEMALE } else { MALE });
                let pt = r.pick(PATR_STEMS);
                [f, pt].iter().filter_map(|s| s.chars().next()).collect()
            }
        };
        let mut li = 0;
        let mut out = Vec::with_capacity(p.toks.len());
        for (i, t) in p.toks.iter().enumerate() {
            let s = match p.roles[i] {
                Role::Surname => self.map_surname(t, female),
                Role::First => self.map_first(t, female),
                Role::Patr => self.map_patr(t, female),
                Role::Initials => t
                    .chars()
                    .map(|c| {
                        if c.is_alphabetic() {
                            let l = initials[li % initials.len().max(1)];
                            li += 1;
                            if c.is_lowercase() { l.to_lowercase().next().unwrap_or(l) } else { l }
                        } else {
                            c
                        }
                    })
                    .collect(),
                Role::Keep => t.clone(),
            };
            out.push(s);
        }
        if with_id {
            let id = self.id_for("person", &id_norm);
            return self.with_id(&out.join(" "), id);
        }
        // Без ID фейк — единственное, что различает людей (ВПР, сводные): при совпадении
        // с чужим берём другое отчество, затем другое имя, в крайнем случае номер.
        let first_pos = p.roles.iter().position(|r| *r == Role::First);
        let patr_pos = p.roles.iter().position(|r| *r == Role::Patr);
        let names = if female { FEMALE } else { MALE };
        let mut r = Rng::new(&[&self.s.salt, "uniq", &id_norm]);
        let (h1, h2) = (r.below(PATR_STEMS.len()), r.below(names.len()));
        let mut fake = out.join(" ");
        let mut k = 0usize;
        let taken = |f: &str, me: &Faker| {
            let n = norm_text(f);
            n != id_norm && me.taken.contains(&n)
        };
        while self.used_person.get(&fake).is_some_and(|owner| *owner != id_norm) || taken(&fake, self) {
            k += 1;
            let mut alt = out.clone();
            if let Some(i) = patr_pos {
                let stem = PATR_STEMS[(h1 + k) % PATR_STEMS.len()];
                alt[i] = apply_case(&p.toks[i], &format!("{stem}{}", if female { "на" } else { "ич" }));
            }
            if let Some(i) = first_pos.filter(|_| patr_pos.is_none() || k >= PATR_STEMS.len()) {
                alt[i] = apply_case(&p.toks[i], names[(h2 + k / PATR_STEMS.len().max(1)) % names.len()]);
            }
            fake = alt.join(" ");
            if k > PATR_STEMS.len() * names.len() {
                fake = format!("{} {k}", out.join(" "));
            }
        }
        self.used_person.insert(fake.clone(), id_norm);
        fake
    }

    fn fallback_person(&mut self, norm: &str, with_id: bool) -> String {
        let mut attempt = 0u32;
        loop {
            let mut r = Rng::new(&[&self.s.salt, "person", norm, &attempt.to_string()]);
            let fake = format!("{} {} {}ич", r.pick(SURNAMES), r.pick(MALE), r.pick(PATR_STEMS));
            if with_id {
                let id = self.id_for("person", norm);
                return self.with_id(&fake, id);
            }
            let clash = self.used_person.get(&fake).is_some_and(|o| o != norm) || self.taken.contains(&norm_text(&fake));
            if !clash || attempt > 1000 {
                self.used_person.insert(fake.clone(), norm.to_string());
                return fake;
            }
            attempt += 1;
        }
    }

    fn fake_name_part(&mut self, kind: Kind, original: &str, with_id: bool) -> String {
        let toks: Vec<&str> = original.split_whitespace().collect();
        if toks.is_empty() {
            return original.to_string();
        }
        let mut out = Vec::new();
        for t in toks {
            let l = norm_word(t);
            if !t.chars().any(|c| c.is_alphabetic()) {
                out.push(t.to_string());
                continue;
            }
            out.push(match kind {
                Kind::LastName => self.map_surname(t, is_female_surname(&l)),
                Kind::FirstName => self.map_first(t, is_female_first(&l)),
                _ => self.map_patr(t, is_female_patronymic(&l)),
            });
        }
        let fake = out.join(" ");
        if with_id {
            let id = self.id_for("person", &norm_text(original));
            self.with_id(&fake, id)
        } else {
            fake
        }
    }

    // ------------------------------------------------ контакты и документы

    /// Несколько email/телефонов в одной ячейке через «;», «,» или перенос строки.
    fn map_multi(&mut self, kind: Kind, original: &str, with_id: bool) -> String {
        let mut out = String::new();
        let mut seg = String::new();
        for c in original.chars() {
            if c == ';' || c == ',' || c == '\n' {
                out.push_str(&self.map_segment(kind, &seg, with_id));
                out.push(c);
                seg.clear();
            } else {
                seg.push(c);
            }
        }
        out.push_str(&self.map_segment(kind, &seg, with_id));
        out
    }

    fn map_segment(&mut self, kind: Kind, seg: &str, with_id: bool) -> String {
        let core = seg.trim();
        if core.is_empty() {
            return seg.to_string();
        }
        let lead = &seg[..seg.len() - seg.trim_start().len()];
        let trail = &seg[seg.trim_end().len()..];
        let fake = match kind {
            Kind::Email => self.fake_email(core, with_id),
            _ => self.fake_phone(core, with_id),
        };
        format!("{lead}{fake}{trail}")
    }

    fn fake_email(&mut self, orig: &str, with_id: bool) -> String {
        let norm = orig.trim().to_lowercase();
        if !norm.contains('@') {
            return self.mask(orig);
        }
        if let Some(e) = self.emails.get(&norm).cloned() {
            self.last_id = self.ids.get("email").and_then(|m| m.get(&norm)).copied();
            return e;
        }
        let e = self.new_email(&norm, with_id);
        self.emails.insert(norm, e.clone());
        e
    }

    fn new_email(&mut self, norm: &str, with_id: bool) -> String {
        let mut r = Rng::new(&[&self.s.salt, "email", norm]);
        let female = r.below(2) == 1;
        let first = r.pick(if female { FEMALE } else { MALE });
        let sur = r.pick(SURNAMES);
        let sur = if female { feminize(sur) } else { sur.to_string() };
        let local = format!("{}.{}", translit(first), translit(&sur));
        let domain = self.s.email_domain.trim().trim_start_matches('@').to_string();
        if with_id {
            let id = self.id_for("email", norm);
            let e = format!("{local}.{id}@{domain}");
            self.used_email.insert(e.clone());
            return e;
        }
        for n in 0.. {
            let num = if n < 20 { 10 + r.below(90) } else { 100 + r.below(9900) };
            let e = format!("{local}{num}@{domain}");
            if self.used_email.insert(e.clone()) {
                return e;
            }
        }
        unreachable!()
    }

    fn fake_phone(&mut self, orig: &str, with_id: bool) -> String {
        let d = digits_of(orig);
        let n = d.len();
        if n < 5 {
            return self.mask(orig);
        }
        let first = d.chars().next().unwrap();
        let key = phone_key(&d);
        if let Some(fk) = self.phones.get(&key).cloned() {
            self.last_id = self.ids.get("phone").and_then(|m| m.get(&key)).copied();
            let digits = if n == 11 && (first == '7' || first == '8') { format!("{first}{fk}") } else { fk };
            return fill_digits(orig, &digits);
        }
        let mut r = Rng::new(&[&self.s.salt, "phone", &key]);
        let (prefix, body_len) = if n == 11 && (first == '7' || first == '8') {
            (first.to_string(), 10)
        } else if n == 10 {
            (String::new(), 10)
        } else {
            (first.to_string(), n - 1)
        };
        let tail_len = if body_len == 10 { 7 } else { body_len };
        let tail = if with_id {
            pad_id(self.id_for("phone", &key), tail_len)
        } else {
            r.digits(tail_len, false)
        };
        let body = if body_len == 10 { format!("900{tail}") } else { tail };
        let out = format!("{prefix}{body}");
        self.phones.insert(key, phone_key(&out));
        fill_digits(orig, &out)
    }

    fn check_digit(d: &[u32], w: &[u32]) -> u32 {
        d.iter().zip(w).map(|(a, b)| a * b).sum::<u32>() % 11 % 10
    }

    /// ИНН, СНИЛС, паспорт, счёт: длинный номер, уже встречавшийся в любом виде и под любым из этих типов,
    /// получает те же фейковые цифры.
    fn fake_number(&mut self, kind: Kind, orig: &str, with_id: bool) -> String {
        // ИНН регионов 01–09 числом: Excel срезает ведущий ноль (274062111 — это 0274062111).
        let padded;
        let t = orig.trim();
        let orig = if kind == Kind::Inn && t.chars().all(|c| c.is_ascii_digit()) && matches!(t.len(), 9 | 11) {
            padded = format!("0{t}");
            padded.as_str()
        } else {
            orig
        };
        let d = digits_of(orig);
        if d.len() >= 9 {
            if let Some(fd) = self.digits.get(&d).cloned() {
                self.last_id = kind.id_ns("").and_then(|ns| self.ids.get(&ns)?.get(&d).copied());
                return fill_digits(orig, &fd);
            }
        }
        let out = match kind {
            Kind::Inn => self.fake_inn(orig, with_id),
            Kind::Snils => self.fake_snils(orig, with_id),
            Kind::Passport => self.fake_passport(orig, with_id),
            _ => self.fake_account(orig, with_id),
        };
        let fd = digits_of(&out);
        if d.len() >= 9 && fd.len() == d.len() {
            self.digits.insert(d, fd);
        }
        out
    }

    fn fake_inn(&mut self, orig: &str, with_id: bool) -> String {
        let d = digits_of(orig);
        if d.len() != 10 && d.len() != 12 {
            return self.fake_account(orig, with_id);
        }
        let mut r = Rng::new(&[&self.s.salt, "inn", &d]);
        let body_len = if d.len() == 10 { 5 } else { 6 };
        let body = if with_id { pad_id(self.id_for("inn", &d), body_len) } else { r.digits(body_len, false) };
        // «99 00 …»: налоговой инспекции с кодом 00 нет — настоящего такого ИНН не бывает.
        // Без ведущих нулей: ИНН-число остаётся числом (ВПР по ИНН не ломается).
        let mut v: Vec<u32> = format!("9900{body}").chars().map(|c| c.to_digit(10).unwrap()).collect();
        if d.len() == 10 {
            v.push(Self::check_digit(&v, &[2, 4, 10, 3, 5, 9, 4, 6, 8]));
        } else {
            v.push(Self::check_digit(&v, &[7, 2, 4, 10, 3, 5, 9, 4, 6, 8]));
            v.push(Self::check_digit(&v, &[3, 7, 2, 4, 10, 3, 5, 9, 4, 6, 8]));
        }
        let s: String = v.iter().map(|x| char::from_digit(*x, 10).unwrap()).collect();
        fill_digits(orig, &s)
    }

    fn fake_snils(&mut self, orig: &str, with_id: bool) -> String {
        let d = digits_of(orig);
        if d.len() != 11 {
            return self.fake_account(orig, with_id);
        }
        let mut r = Rng::new(&[&self.s.salt, "snils", &d]);
        let base = if with_id {
            format!("{:09}", 100_000_000 + self.id_for("snils", &d) % 900_000_000)
        } else {
            r.digits(9, true)
        };
        let sum: u32 = base.chars().enumerate().map(|(i, c)| c.to_digit(10).unwrap() * (9 - i as u32)).sum();
        let check = match sum {
            s if s < 100 => s,
            100 | 101 => 0,
            s => {
                let m = s % 101;
                if m == 100 { 0 } else { m }
            }
        };
        fill_digits(orig, &format!("{base}{check:02}"))
    }

    fn fake_passport(&mut self, orig: &str, with_id: bool) -> String {
        let d = digits_of(orig);
        if d.len() != 10 {
            return self.fake_account(orig, with_id);
        }
        let mut r = Rng::new(&[&self.s.salt, "passport", &d]);
        let s = if with_id { format!("9900{}", pad_id(self.id_for("passport", &d), 6)) } else { format!("99{}", r.digits(8, false)) };
        fill_digits(orig, &s)
    }

    fn fake_account(&mut self, orig: &str, with_id: bool) -> String {
        let d = digits_of(orig);
        if d.is_empty() {
            return self.mask(orig);
        }
        if short_code(&d) {
            // Шесть цифр на «9»: фейк длиннее любого короткого кода и не совпадёт ни с одним настоящим.
            // ID — по всему коду: «Дог1» и «Счет1» — разные вещи.
            let tail = if with_id {
                let id = self.id_for("code", &norm_text(orig));
                if id < 100_000 { format!("9{id:05}") } else { format!("9{id}") }
            } else {
                format!("9{}", Rng::new(&[&self.s.salt, "code", &norm_text(orig)]).digits(5, false))
            };
            return replace_digit_run(orig, &tail);
        }
        let keep = if d.len() >= 12 { 4 } else { 0 };
        let rest = d.len() - keep;
        let mut r = Rng::new(&[&self.s.salt, "account", &d]);
        let mut tail = if with_id { pad_id(self.id_for("account", &d), rest) } else { r.digits(rest, keep == 0 && !d.starts_with('0')) };
        if keep == 0 && !d.starts_with('0') && tail.starts_with('0') && tail.len() > 1 {
            // Номер без ведущего нуля — и фейк без него, иначе число станет текстом.
            tail.replace_range(0..1, "9");
        }
        fill_digits(orig, &format!("{}{}", &d[..keep], tail))
    }

    fn fake_address(&mut self, orig: &str, with_id: bool) -> String {
        let norm = norm_text(strip_id_suffix(orig));
        let s = match self.addresses.get(&norm) {
            Some(a) => a.clone(),
            None => {
                let a = self.new_address(orig, &norm);
                self.addresses.insert(norm.clone(), a.clone());
                a
            }
        };
        if with_id {
            let id = self.id_for("address", &norm);
            self.with_id(&s, id)
        } else {
            s
        }
    }

    fn new_address(&mut self, orig: &str, norm: &str) -> String {
        let mut r = Rng::new(&[&self.s.salt, "addr", norm]);
        let mut s = String::new();
        let first = orig.split([',', ' ']).next().unwrap_or("");
        if first.len() == 6 && first.chars().all(|c| c.is_ascii_digit()) {
            s.push_str(&format!("{}, ", 100000 + r.below(900000)));
        }
        s.push_str(&format!("г. {}, ул. {}, д. {}", r.pick(CITIES), r.pick(STREETS), 1 + r.below(120)));
        if norm.contains("кв") {
            s.push_str(&format!(", кв. {}", 1 + r.below(250)));
        }
        s
    }

    fn fake_company(&mut self, orig: &str, with_id: bool) -> String {
        let (pre, name, post) = split_company(strip_id_suffix(orig));
        let key = norm_text(&name);
        let (pre, post) = (form_only(&pre), form_only(&post));
        let s = if is_ip(&pre, &post) {
            // ИП — это человек: то же фейковое ФИО, что и в колонке ФИО.
            let p = self.fake_person(&name, false);
            if name.chars().count() >= 4 {
                // То же ФИО без «ИП» в другой колонке или в тексте («Ответственный: Козлов О.П.»).
                // Без ID: номер ИП — из нумерации организаций, у людей своя.
                self.company_text.entry(name.trim().to_string()).or_insert_with(|| (p.clone(), None));
            }
            format!("{pre}{p}")
        } else {
            let fake = match self.company.get(&key) {
                Some(f) => f.clone(),
                None => {
                    let f = self.new_company_name(&key);
                    self.company.insert(key.clone(), f.clone());
                    f
                }
            };
            let fake = apply_case(&name, &fake);
            if name.chars().count() >= 3 {
                // Название в любом написании и внутри текста получает тот же ID, что и в колонке.
                let id = with_id.then(|| self.peek_id("company", &key));
                self.company_text.entry(name.trim().to_string()).or_insert_with(|| (fake.clone(), id));
            }
            format!("{pre}{fake}{post}")
        };
        if with_id {
            let id = self.id_for("company", &key);
            self.with_id(&s, id)
        } else {
            s
        }
    }

    fn new_company_name(&mut self, key: &str) -> String {
        let mut r = Rng::new(&[&self.s.salt, "company", key]);
        let mut name = String::new();
        for attempt in 0..400 {
            name = if attempt < 20 {
                r.pick(COMPANY_NAMES).to_string()
            } else if attempt < 150 {
                format!("{} {}", r.pick(COMPANY_NAMES), r.pick(COMPANY_SUFFIXES))
            } else {
                format!("{} {}", r.pick(COMPANY_NAMES), 2 + r.below(9998))
            };
            let n = norm_text(&name);
            if !self.used_company.contains(&n) && !self.taken.contains(&n) && n != key {
                break;
            }
        }
        self.used_company.insert(norm_text(&name));
        name
    }

    // ------------------------------------------------ прочее

    fn fake_date(&mut self, orig: &str, numeric: bool) -> String {
        let day = date_day(orig, numeric);
        let offset = match day.and_then(|d| self.date_shift.get(&d)) {
            Some(&o) => o,
            None => {
                let seed = day.map_or_else(|| orig.trim().to_string(), |d| d.to_string());
                let mut r = Rng::new(&[&self.s.salt, "date", &seed]);
                let mut o = r.below(361) as i64 - 180;
                if o == 0 {
                    o = 7;
                }
                if let Some(d) = day {
                    self.date_shift.insert(d, o);
                }
                o
            }
        };
        if numeric {
            if let Ok(v) = orig.trim().parse::<f64>() {
                let nv = v + offset as f64;
                return if nv.fract() == 0.0 { format!("{}", nv as i64) } else { format!("{nv}") };
            }
        }
        shift_text_date(orig.trim(), offset).unwrap_or_else(|| self.mask(orig))
    }

    /// Случайные символы того же вида: цифра → цифра, кириллица → кириллица и т.д.
    fn mask(&self, orig: &str) -> String {
        const CYR: &[char] = &['а', 'б', 'в', 'г', 'д', 'е', 'ж', 'з', 'и', 'к', 'л', 'м', 'н', 'о', 'п', 'р', 'с', 'т', 'у', 'ф', 'х', 'ц', 'ч', 'ш', 'э', 'ю', 'я'];
        let mut r = Rng::new(&[&self.s.salt, "mask", orig]);
        let mut first_digit = true;
        orig.chars()
            .map(|c| {
                if c.is_ascii_digit() {
                    let nz = first_digit && c != '0';
                    first_digit = false;
                    let d = if nz { 1 + r.below(9) } else { r.below(10) };
                    (b'0' + d as u8) as char
                } else if c.is_ascii_alphabetic() {
                    let l = (b'a' + r.below(26) as u8) as char;
                    if c.is_ascii_uppercase() { l.to_ascii_uppercase() } else { l }
                } else if ('а'..='я').contains(&c.to_lowercase().next().unwrap_or(c)) || c == 'ё' || c == 'Ё' {
                    let l = CYR[r.below(CYR.len())];
                    if c.is_uppercase() { l.to_uppercase().next().unwrap_or(l) } else { l }
                } else {
                    c
                }
            })
            .collect()
    }

    // ------------------------------------------------ загрузка ключа прошлых запусков

    /// Восстанавливает соответствия из ключа: те же люди получат те же ID и фейки.
    pub fn load_entries(&mut self, entries: Vec<KeyEntry>) {
        for e in entries {
            let key = (e.kind_key.clone(), e.real.clone());
            if self.cache.contains_key(&key) {
                continue;
            }
            if let Some((kind, _, label)) = parse_kind_key(&e.kind_key) {
                let number = matches!(kind, Kind::Inn | Kind::Snils | Kind::Passport | Kind::Account);
                let short = number && short_code(&digits_of(&e.real));
                // Ключ версий до 0.3: короткий код получал ID на место своих цифр («Дог1» → «Дог1») —
                // такую запись не восстанавливаем, код получит новый фейк.
                if short && digits_of(&e.fake).len() == digits_of(&e.real).len() {
                    continue;
                }
                if let (Some(ns), Some(id)) = (kind.id_ns(&label), e.id) {
                    let ns = if short { "code".to_string() } else { ns };
                    let norm = match kind {
                        _ if short => norm_text(&e.real),
                        Kind::Phone => phone_key(&e.real),
                        Kind::Inn | Kind::Snils | Kind::Passport | Kind::Account => digits_of(&e.real),
                        Kind::Company => company_key(&e.real),
                        Kind::Email => e.real.trim().to_lowercase(),
                        _ => norm_text(&e.real),
                    };
                    // для телефонов и email в ячейке может быть несколько значений — ID не восстанавливаем
                    let single = !matches!(kind, Kind::Phone | Kind::Email) || !e.real.contains([';', ',', '\n']);
                    if single {
                        self.ids.entry(ns.clone()).or_default().entry(norm).or_insert(id);
                        let next = self.next_id.entry(ns).or_insert(1);
                        *next = (*next).max(id + 1);
                    }
                }
                match kind {
                    Kind::FullName => {
                        let fake_core = e.id.map_or(e.fake.clone(), |id| self.strip_id(&e.fake, id));
                        self.register_person(&e.real, &fake_core);
                        // «Иванов И.И.» в новом файле привяжется к полному ФИО из ключа.
                        self.prepare_people(std::iter::once(e.real.as_str()));
                        if e.id.is_none() {
                            self.used_person.insert(e.fake.clone(), norm_text(strip_id_suffix(&e.real)));
                        }
                    }
                    Kind::Company => {
                        let fake_core = e.id.map_or(e.fake.clone(), |id| self.strip_id(&e.fake, id));
                        let (rp, rn, rpost) = split_company(strip_id_suffix(&e.real));
                        let (fp, fname, _) = split_company(&fake_core);
                        if is_ip(&rp, &rpost) {
                            self.register_person(&rn, &fname);
                            if rn.chars().count() >= 4 {
                                self.company_text.entry(rn.trim().to_string()).or_insert_with(|| (fname.clone(), None));
                            }
                            if e.id.is_none() {
                                self.used_person.insert(fname.clone(), norm_text(&rn));
                            }
                        } else if !fp.is_empty() || !fname.is_empty() {
                            self.used_company.insert(norm_text(&fname));
                            // Фейк записан в ключ ЗАГЛАВНЫМИ (исходник был заглавными) — для другого написания нужен обычный вид.
                            let base = if fname.chars().any(char::is_lowercase) { fname.clone() } else { title_case(&fname) };
                            self.company.entry(norm_text(&rn)).or_insert(base);
                            if rn.chars().count() >= 3 {
                                self.company_text.entry(rn.trim().to_string()).or_insert((fname, e.id));
                            }
                        }
                    }
                    Kind::Inn | Kind::Snils | Kind::Passport | Kind::Account => {
                        let (d, fd) = (digits_of(&e.real), digits_of(&e.fake));
                        if d.len() >= 9 && d.len() == fd.len() {
                            self.digits.entry(d).or_insert(fd);
                        }
                    }
                    Kind::LastName | Kind::FirstName | Kind::MiddleName => {
                        let fake_core = e.id.map_or(e.fake.clone(), |id| self.strip_id(&e.fake, id));
                        self.register_part(kind, &e.real, &fake_core);
                    }
                    Kind::Email | Kind::Phone => {
                        // В ячейке может быть несколько значений через «;» — сопоставляем попарно.
                        let split = |s: &str| s.split([';', ',', '\n']).map(|x| x.trim().to_string()).collect::<Vec<_>>();
                        let (rs, fs) = (split(&e.real), split(&e.fake));
                        if rs.len() == fs.len() {
                            for (r, f) in rs.iter().zip(&fs).filter(|(r, _)| !r.is_empty()) {
                                if kind == Kind::Email {
                                    self.used_email.insert(f.clone());
                                    self.emails.entry(r.to_lowercase()).or_insert_with(|| f.clone());
                                } else {
                                    let (dr, df) = (digits_of(r), digits_of(f));
                                    if dr.len() == df.len() && dr.len() >= 5 {
                                        self.phones.entry(phone_key(&dr)).or_insert_with(|| phone_key(&df));
                                    }
                                }
                            }
                        }
                    }
                    Kind::Address => {
                        let fake_core = e.id.map_or(e.fake.clone(), |id| self.strip_id(&e.fake, id));
                        self.addresses.entry(norm_text(strip_id_suffix(&e.real))).or_insert(fake_core);
                    }
                    Kind::BirthDate => {
                        if let (Some(r), Some(f)) = (date_day(&e.real, false), date_day(&e.fake, false)) {
                            if r != f {
                                self.date_shift.entry(r).or_insert(f - r);
                            }
                        }
                    }
                    _ => {}
                }
            }
            if e.id.is_some() && !e.fake.is_empty() {
                self.known_fakes.insert(e.fake.clone());
            }
            self.entries.push(e);
            self.cache.insert(key, self.entries.len() - 1);
        }
    }

    fn register_token(&mut self, role: Role, real: &str, fake: &str, female: bool) {
        let fake = title_case(fake);
        match role {
            Role::Surname => {
                let base = masculinize(&fake, female);
                self.used_surname.insert(norm_word(&base));
                self.surname.entry(masculinize(&norm_word(real), female)).or_insert(base);
            }
            Role::First => {
                let g = female as usize;
                self.used_first[g].insert(norm_word(&fake));
                self.first[g].entry(norm_word(real)).or_insert(fake);
            }
            Role::Patr => {
                let fk = patr_key(&norm_word(&fake));
                if let Some(i) = PATR_STEMS.iter().position(|s| norm_word(s) == fk) {
                    self.used_patr.insert(i);
                    self.patr.entry(patr_key(&norm_word(real))).or_insert(i);
                }
            }
            _ => {}
        }
    }

    fn register_person(&mut self, real: &str, fake: &str) {
        let (Some(pr), Some(pf)) = (parse_person(real), parse_person(fake)) else { return };
        if pr.toks.len() != pf.toks.len() {
            return;
        }
        for i in 0..pr.toks.len() {
            self.register_token(pr.roles[i], &pr.toks[i], &pf.toks[i], pr.female);
        }
    }

    fn register_part(&mut self, kind: Kind, real: &str, fake: &str) {
        let r: Vec<&str> = real.split_whitespace().collect();
        let f: Vec<&str> = fake.split_whitespace().collect();
        if r.len() != f.len() {
            return;
        }
        for (rt, ft) in r.iter().zip(&f) {
            let l = norm_word(rt);
            let (role, female) = match kind {
                Kind::LastName => (Role::Surname, is_female_surname(&l)),
                Kind::FirstName => (Role::First, is_female_first(&l)),
                _ => (Role::Patr, is_female_patronymic(&l)),
            };
            self.register_token(role, rt, ft, female);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn faker() -> Faker {
        Faker::new(&Settings { salt: "test".into(), ..Settings::default() })
    }

    fn rule(kind: Kind) -> Rule {
        Rule::new("Лист1", 0, 1, None, kind)
    }

    #[test]
    fn person_keeps_gender_and_family() {
        let mut f = faker();
        let fio = rule(Kind::FullName);
        let m = f.fake(&fio, "Иванов Иван Иванович", false);
        let w = f.fake(&fio, "Иванова Мария Петровна", false);
        let ms: Vec<&str> = m.split_whitespace().collect();
        let ws: Vec<&str> = w.split_whitespace().collect();
        assert_eq!(feminize(ms[0]), ws[0], "{m} / {w}");
        assert!(ms[2].ends_with("ич"));
        assert!(ws[2].ends_with("на"));
        assert!(m.ends_with("(ID 1)"));
        assert!(w.ends_with("(ID 2)"));
        // повтор — тот же результат
        assert_eq!(f.fake(&fio, "Иванов Иван Иванович", false), m);
        // фамилия отдельно — та же фейковая
        let last = f.fake(&rule(Kind::LastName), "Иванов", false);
        assert_eq!(last, ms[0]);
    }

    #[test]
    fn initials_link_to_full_name() {
        let mut f = faker();
        f.prepare_people(["Сидоров Пётр Алексеевич"]);
        let fio = rule(Kind::FullName);
        let full = f.fake(&fio, "Сидоров Пётр Алексеевич", false);
        let short = f.fake(&fio, "Сидоров П.А.", false);
        let fs: Vec<&str> = full.split_whitespace().collect();
        let expected = format!(
            "{} {}.{}. (ID 1)",
            fs[0],
            fs[1].chars().next().unwrap(),
            fs[2].chars().next().unwrap()
        );
        assert_eq!(short, expected);
    }

    #[test]
    fn reanonymize_and_parenthesis() {
        let mut f = faker();
        let fio = rule(Kind::FullName);
        let a = f.fake(&fio, "Герасимова Регина (ID 3)", false);
        let p: Vec<&str> = a.split_whitespace().collect();
        assert_eq!(p.len(), 4, "{a}");
        assert!(is_female_surname(&norm_word(p[0])) && FEMALE.contains(&p[1]), "{a}");
        assert!(a.ends_with("(ID 1)"));
        // та же, но без приписки — тот же человек
        assert_eq!(f.fake(&fio, "Герасимова Регина", false), a);
        let b = f.fake(&fio, "Петров Пётр (стажёр)", false);
        assert!(b.contains(" (стажёр) (ID 2)") || b.ends_with("(стажёр) (ID 2)"), "{b}");
    }

    #[test]
    fn phone_keeps_format_and_id() {
        let mut f = faker();
        let ph = rule(Kind::Phone);
        assert_eq!(f.fake(&ph, "+7 (916) 123-45-67", false), "+7 (900) 000-00-01");
        // тот же номер в другом формате — тот же ID
        assert_eq!(f.fake(&ph, "89161234567", true), "89000000001");
    }

    #[test]
    fn inn_checksum_valid() {
        let mut f = faker();
        let inn = f.fake(&rule(Kind::Inn), "7707083893", true);
        let d: Vec<u32> = inn.chars().map(|c| c.to_digit(10).unwrap()).collect();
        assert_eq!(Faker::check_digit(&d[..9], &[2, 4, 10, 3, 5, 9, 4, 6, 8]), d[9]);
        assert!(inn.starts_with("9900"), "{inn}");
    }

    #[test]
    fn email_has_id() {
        let mut f = faker();
        let e = f.fake(&rule(Kind::Email), "ivan@corp.ru; petr@corp.ru", false);
        assert!(e.contains(".1@example.com"), "{e}");
        assert!(e.contains("; ") && e.contains(".2@example.com"), "{e}");
    }

    #[test]
    fn date_shift() {
        assert_eq!(shift_text_date("31.12.1990", 1).unwrap(), "01.01.1991");
        assert_eq!(shift_text_date("1990-03-01", -1).unwrap(), "1990-02-28");
    }

    #[test]
    fn short_code_never_maps_to_itself() {
        let mut f = faker();
        for w in [true, false] {
            let mut r = rule(Kind::Account);
            r.with_id = w;
            let reals = ["Дог1", "Дог2", "Дог3", "Счет1", "Налог10", "Налог1", "1", "А-12/3", "Код1234"];
            let fakes: Vec<String> = reals.iter().map(|x| f.fake(&r, x, false)).collect();
            for (x, y) in reals.iter().zip(&fakes) {
                assert!(!reals.contains(&y.as_str()), "{x} → {y}: фейк совпал с настоящим кодом");
                assert_eq!(digits_of(y).len(), 6, "{x} → {y}");
            }
            let uniq: std::collections::HashSet<&String> = fakes.iter().collect();
            assert_eq!(uniq.len(), fakes.len(), "разные коды — разные фейки: {fakes:?}");
            assert!(fakes[0].starts_with("Дог9") && fakes[7].starts_with("А-9") && fakes[7].ends_with('/'), "{fakes:?}");
        }
        let mut r = rule(Kind::Account);
        r.with_id = true;
        assert_eq!(f.fake(&r, "Счет 5", false), "Счет 900010", "ID по всему коду, следующий после девяти");
    }

    #[test]
    fn account_keeps_numeric_shape() {
        let mut f = faker();
        for w in [true, false] {
            let mut r = rule(Kind::Account);
            r.with_id = w;
            let a = f.fake(&r, "12345678901", true);
            assert_eq!(a.len(), 11, "{a}");
            assert!(!a.starts_with('0'), "номер-число стал бы текстом: {a}");
        }
    }

    #[test]
    fn company_variants_one_name() {
        for (s, parts) in [
            ("ООО «Ромашка»", ("ООО «", "Ромашка", "»")),
            ("ООО \"Ромашка\"", ("ООО \"", "Ромашка", "\"")),
            ("Ромашка, ООО", ("", "Ромашка", ", ООО")),
            ("Вектор АО", ("", "Вектор", " АО")),
            ("ИП Козлов О.П.", ("ИП ", "Козлов О.П.", "")),
            ("Общество с ограниченной ответственностью «Ромашка» (ID 4)", ("Общество с ограниченной ответственностью «", "Ромашка", "» (ID 4)")),
        ] {
            let (a, b, c) = split_company(s);
            assert_eq!((a.as_str(), b.as_str(), c.as_str()), parts, "{s}");
        }
        assert_eq!(company_key("ООО «РОМАШКА» (ID 4)"), company_key("Ромашка"));
    }

    #[test]
    fn company_keeps_form() {
        let mut f = faker();
        let c = f.fake(&rule(Kind::Company), "ООО «Ромашка»", false);
        assert!(c.starts_with("ООО «") && c.ends_with("(ID 1)"), "{c}");
    }

    /// «(М) ООО "Восход"» → «(М) ООО "Прогресс"»: пометка перед формой (и город, филиал после кавычек)
    /// переносилась в фейк как есть. Снаружи названия остаются только форма и знаки.
    #[test]
    fn company_drops_words_outside_name() {
        let mut f = faker();
        let r = rule(Kind::Company);
        for (orig, starts, ends) in [
            ("(М) ООО \"Восход\"", "ООО \"", "\" (ID 1)"),
            ("Филиал №5 ООО «Восход» (Москва)", "ООО «", "» (ID 1)"),
            ("Общество с ограниченной ответственностью «Восход» г. Тверь", "Общество с ограниченной ответственностью «", "» (ID 1)"),
            ("Склад \"Восход\"", "\"", "\" (ID 1)"),
            ("Восход, ООО", "", ", ООО (ID 1)"),
        ] {
            let c = f.fake(&r, orig, false);
            assert!(c.starts_with(starts) && c.ends_with(ends), "{orig} → {c}");
            for w in ["(М)", "Филиал", "№5", "Москва", "Тверь", "Склад", "Восход"] {
                assert!(!c.contains(w), "{orig} → {c}: осталось «{w}»");
            }
        }
    }

    #[test]
    fn key_reload_keeps_ids() {
        let mut f = faker();
        let fio = rule(Kind::FullName);
        let a = f.fake(&fio, "Кузнецов Олег Игоревич", false);
        let entries = f.entries.clone();
        let mut g = Faker::new(&Settings { salt: "другая".into(), ..Settings::default() });
        g.load_entries(entries);
        assert_eq!(g.fake(&fio, "Кузнецов Олег Игоревич", false), a);
        let b = g.fake(&fio, "Новый Человек Тестович", false);
        assert!(b.ends_with("(ID 2)"), "{b}");
        // фамилия согласована с загруженным ФИО
        let last = g.fake(&rule(Kind::LastName), "Кузнецова", false);
        assert_eq!(last, feminize(a.split_whitespace().next().unwrap()));
    }
}
