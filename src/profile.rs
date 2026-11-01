//! Профиль обезличивания: правила (лист + колонка + диапазон строк + тип) и настройки.
//! Сохраняется в JSON, чтобы повторять обработку новых версий того же файла.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::xlsx::col_name;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Kind {
    FullName,
    LastName,
    FirstName,
    MiddleName,
    Email,
    Phone,
    Inn,
    Snils,
    Passport,
    Address,
    BirthDate,
    Company,
    Account,
    Mask,
    Text,
    Clear,
    Keep,
}

impl Kind {
    pub const ALL: [Kind; 17] = [
        Kind::FullName,
        Kind::LastName,
        Kind::FirstName,
        Kind::MiddleName,
        Kind::Email,
        Kind::Phone,
        Kind::Inn,
        Kind::Snils,
        Kind::Passport,
        Kind::Address,
        Kind::BirthDate,
        Kind::Company,
        Kind::Account,
        Kind::Mask,
        Kind::Text,
        Kind::Clear,
        Kind::Keep,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Kind::FullName => "ФИО",
            Kind::LastName => "Фамилия",
            Kind::FirstName => "Имя",
            Kind::MiddleName => "Отчество",
            Kind::Email => "Email",
            Kind::Phone => "Телефон",
            Kind::Inn => "ИНН",
            Kind::Snils => "СНИЛС",
            Kind::Passport => "Паспорт",
            Kind::Address => "Адрес",
            Kind::BirthDate => "Дата рождения",
            Kind::Company => "Организация",
            Kind::Account => "Номер (счёт/карта/договор)",
            Kind::Mask => "Маска (сохранить формат)",
            Kind::Text => "Шаблон: подпись + номер",
            Kind::Clear => "Очистить",
            Kind::Keep => "Не менять (защитить)",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Kind::FullName => "Иванов Иван Иванович → Петров Олег Сергеевич (ID 12). Пол и формат (инициалы, порядок) сохраняются",
            Kind::LastName => "Та же фамилия, что и в ФИО: Иванова → Петрова",
            Kind::FirstName => "Имя с сохранением пола",
            Kind::MiddleName => "Отчество с сохранением пола",
            Kind::Email => "oleg.petrov.12@example.com — ID внутри адреса",
            Kind::Phone => "+7 (900) 000-00-12 — формат сохраняется, ID в последних цифрах",
            Kind::Inn => "00000001239 — ID внутри, контрольные цифры корректные",
            Kind::Snils => "100-000-012 34 — ID внутри, контрольное число корректное",
            Kind::Passport => "00 00 000012 — ID в номере",
            Kind::Address => "г. Казань, ул. Садовая, д. 5 (ID 12)",
            Kind::BirthDate => "Сдвиг даты на ±180 дней (одинаковый для одинаковых значений)",
            Kind::Company => "ООО «Вектор» (ID 12) — организационная форма сохраняется",
            Kind::Account => "Цифры заменяются на ID с ведущими нулями, формат сохраняется",
            Kind::Mask => "Случайные цифры/буквы в том же формате, без ID",
            Kind::Text => "Каждое разное значение → «подпись номер»: «Должность 1», «Налог 2». Подпись берётся из заголовка колонки, её можно изменить",
            Kind::Clear => "Пустое значение",
            Kind::Keep => "Ячейки остаются как есть, даже если такие же значения заменяются в других местах",
        }
    }

    /// Можно ли включать/выключать ID для этого типа.
    pub fn id_optional(self) -> bool {
        !matches!(self, Kind::BirthDate | Kind::Mask | Kind::Text | Kind::Clear | Kind::Keep)
    }

    pub fn default_with_id(self) -> bool {
        !matches!(
            self,
            Kind::LastName | Kind::FirstName | Kind::MiddleName | Kind::BirthDate | Kind::Mask | Kind::Clear | Kind::Keep
        )
    }

    /// Пространство нумерации ID. Одинаковое значение в одном пространстве получает один ID.
    pub fn id_ns(self, label: &str) -> Option<String> {
        let ns = match self {
            Kind::FullName | Kind::LastName | Kind::FirstName | Kind::MiddleName => "person",
            Kind::Email => "email",
            Kind::Phone => "phone",
            Kind::Inn => "inn",
            Kind::Snils => "snils",
            Kind::Passport => "passport",
            Kind::Address => "address",
            Kind::Company => "company",
            Kind::Account => "account",
            Kind::Text => return Some(format!("text:{label}")),
            Kind::BirthDate | Kind::Mask | Kind::Clear | Kind::Keep => return None,
        };
        Some(ns.to_string())
    }

    /// Имя типа в файле-ключе. Не меняется вместе с названием в окне — иначе старые ключи не прочитаются.
    pub fn key_name(self) -> &'static str {
        match self {
            Kind::Text => "Текст + ID",
            k => k.title(),
        }
    }

    pub fn from_title(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.title() == s || k.key_name() == s)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub sheet: String,
    /// Номер колонки с 0 (A = 0).
    pub col: u32,
    /// Первая строка (с 1), включительно.
    pub row_from: u32,
    /// Последняя строка включительно; `None` — до конца листа.
    pub row_to: Option<u32>,
    pub kind: Kind,
    #[serde(default)]
    pub with_id: bool,
    /// Подпись для типа «Шаблон».
    #[serde(default)]
    pub label: String,
    /// «Связь»: слово, которое отделяет колонку от остальных. Пусто — общая связь (как до 0.5.0):
    /// одно значение одного типа везде получает один фейк. У колонок со словом «контрагент» —
    /// свои фейки: «Москва» в них и «Москва» в колонках без слова получат разные фейки,
    /// а между собой (на любых листах и в любых файлах) — один.
    /// В профиль пишется, только если заполнено: старые программы и старые профили его не видят.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub link: String,
}

/// Имя связи, как оно идёт в ключ и в генератор фейков: без лишних пробелов, строчными
/// («Контрагент» и «контрагент » — одна связь), без «@», «[», «]» — они разделяют части ключа типа.
pub fn link_name(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !matches!(c, '@' | '[' | ']') && !c.is_control())
        .collect::<String>()
        .trim()
        .to_lowercase()
}

/// Ключ типа без связи и связь: «ФИО+ID@контрагент» → («ФИО+ID», «контрагент»); без «@» — связь пустая.
/// «@» внутри подписи шаблона («Текст + ID[a@b]») связью не считается: после связи нет «]».
pub fn split_link(key: &str) -> (&str, &str) {
    match key.rsplit_once('@') {
        Some((head, link)) if !head.is_empty() && !link.contains(']') => (head, link),
        _ => (key, ""),
    }
}

impl Rule {
    pub fn new(sheet: &str, col: u32, row_from: u32, row_to: Option<u32>, kind: Kind) -> Rule {
        Rule {
            sheet: sheet.to_string(),
            col,
            row_from: row_from.max(1),
            row_to,
            kind,
            with_id: kind.default_with_id(),
            label: String::new(),
            link: String::new(),
        }
    }

    /// Связь правила в виде для ключа и генератора (см. `link_name`); пусто — общая.
    pub fn link_name(&self) -> String {
        link_name(&self.link)
    }

    pub fn covers(&self, row: u32, col: u32) -> bool {
        col == self.col && row >= self.row_from && self.row_to.map_or(true, |to| row <= to)
    }

    pub fn range_text(&self) -> String {
        let c = col_name(self.col);
        match self.row_to {
            Some(to) => format!("{c}{}:{c}{to}", self.row_from),
            None => format!("{c}{}:{c}…", self.row_from),
        }
    }

    pub fn label_or_default(&self) -> &str {
        if self.label.trim().is_empty() {
            "Значение"
        } else {
            self.label.trim()
        }
    }

    /// Ячейка с подписью «Шаблона» — это заголовок колонки, попавший в диапазон, а не значение.
    pub fn is_template_header(&self, text: &str) -> bool {
        self.kind == Kind::Text && !self.label.trim().is_empty() && text.trim().to_lowercase() == self.label.trim().to_lowercase()
    }

    pub fn with_id_effective(&self) -> bool {
        match self.kind {
            Kind::Text => true,
            k if k.id_optional() => self.with_id,
            _ => false,
        }
    }

    /// Ключ типа замены: одно и то же значение под одним ключом всегда даёт один фейк.
    /// Пишется в колонку «Тип» файла-ключа. Связь — в конце через «@» и только непустая:
    /// ключи без связи те же, что до 0.5.0.
    pub fn kind_key(&self) -> String {
        let mut s = self.kind.key_name().to_string();
        if self.kind.id_optional() && self.with_id {
            s.push_str("+ID");
        }
        if self.kind == Kind::Text {
            s.push_str(&format!("[{}]", self.label_or_default()));
        }
        let link = self.link_name();
        if !link.is_empty() {
            s.push('@');
            s.push_str(&link);
        }
        s
    }
}

/// Разбирает ключ типа обратно: (тип, с ID, подпись). Связь отбрасывается — её даёт `split_link`.
pub fn parse_kind_key(key: &str) -> Option<(Kind, bool, String)> {
    let key = split_link(key).0;
    let (head, label) = match key.find('[') {
        Some(i) if key.ends_with(']') => (&key[..i], key[i + 1..key.len() - 1].to_string()),
        _ => (key, String::new()),
    };
    let (title, with_id) = match head.strip_suffix("+ID") {
        Some(t) => (t, true),
        None => (head, false),
    };
    Kind::from_title(title).map(|k| (k, with_id || k == Kind::Text, label))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    /// Шаблон добавления ID к ФИО, адресу, организации: `{fake}` и `{id}`.
    pub id_format: String,
    /// Домен для фейковых email. example.com зарезервирован (RFC 2606) — письма никуда не уйдут.
    pub email_domain: String,
    /// Соль для выбора фейков: случайная на каждый запуск и в профиль не пишется,
    /// чтобы по профилю нельзя было подобрать фейки для подозреваемых людей.
    /// Повторяемость между запусками даёт только файл-ключ.
    #[serde(skip, default = "random_salt")]
    pub salt: String,
    /// Заменить автора/компанию в свойствах файла, авторов примечаний.
    pub clean_metadata: bool,
    /// Попросить Excel пересчитать формулы при открытии.
    pub recalc_on_open: bool,
    /// Заменять ФИО/email/телефоны и внутри длинного текста и строк в формулах.
    /// Прежняя галочка; теперь — часть `outside` (выключенная = «только целые ячейки»).
    #[serde(default = "yes")]
    pub inner_text: bool,
    /// Что делать со значениями вне разметки.
    #[serde(default)]
    pub outside: Outside,
}

/// Значения, размеченные где-то правилом, в ячейках вне разметки.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Outside {
    /// Заменять везде: целые ячейки и внутри текста и формул.
    #[default]
    All,
    /// Только ячейки, где значение стоит целиком.
    Cells,
    /// Ячейки вне разметки не трогать.
    Keep,
}

impl Outside {
    pub const ALL: [Outside; 3] = [Outside::All, Outside::Cells, Outside::Keep];

    pub fn title(self) -> &'static str {
        match self {
            Outside::All => "заменять везде",
            Outside::Cells => "только целые ячейки",
            Outside::Keep => "не трогать",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Outside::All => "Те же ФИО, организации, ИНН, телефоны заменяются во всех ячейках, внутри текста («Оплата ООО «Ромашка»»), формулах и примечаниях",
            Outside::Cells => "Заменяются только ячейки, где значение стоит целиком; внутри текста и формул — нет",
            Outside::Keep => "Ячейки без правил остаются как есть, их формулы тоже — результат формулы вроде СЧЁТЕСЛИ по ФИО может измениться. Сводные, диаграммы и примечания с теми же значениями всё равно заменяются — иначе в них останутся реальные данные",
        }
    }
}

impl Settings {
    /// Искать значения внутри текста, формул, примечаний.
    pub fn inner_search(&self) -> bool {
        self.inner_text && self.outside == Outside::All
    }
}

fn yes() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            id_format: "{fake} (ID {id})".to_string(),
            email_domain: "example.com".to_string(),
            salt: random_salt(),
            clean_metadata: true,
            recalc_on_open: true,
            inner_text: true,
            outside: Outside::All,
        }
    }
}

fn random_salt() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut h: u64 = 0xcbf29ce484222325 ^ std::process::id() as u64;
    for b in nanos.to_le_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Profile {
    pub rules: Vec<Rule>,
    #[serde(default)]
    pub settings: Settings,
}

impl Profile {
    /// Добавляет правила. Пересекающееся правило той же колонки: при `replace` заменяется
    /// новым (так диапазон сужают или расширяют повторным выделением), иначе новое пропускается.
    /// Возвращает (добавлено, заменено).
    pub fn add_rules(&mut self, rules: Vec<Rule>, replace: bool) -> (usize, usize) {
        let (mut added, mut replaced) = (0, 0);
        for r in rules {
            let overlaps = |x: &Rule| {
                x.sheet == r.sheet
                    && x.col == r.col
                    && x.row_from <= r.row_to.unwrap_or(u32::MAX)
                    && r.row_from <= x.row_to.unwrap_or(u32::MAX)
            };
            match self.rules.iter().position(|x| overlaps(x)) {
                Some(_) if !replace => continue,
                Some(i) => {
                    let before = self.rules.len();
                    self.rules.retain(|x| !overlaps(x));
                    replaced += before - self.rules.len();
                    let at = i.min(self.rules.len());
                    self.rules.insert(at, r);
                }
                None => {
                    self.rules.push(r);
                    added += 1;
                }
            }
        }
        (added, replaced)
    }

    pub fn load(path: &Path) -> Result<Profile> {
        let text = std::fs::read_to_string(path).with_context(|| format!("не читается {}", path.display()))?;
        serde_json::from_str(text.trim_start_matches('\u{feff}')).context("профиль: неверный JSON")
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self)?)
            .with_context(|| format!("не записывается {}", path.display()))
    }
}

// ---------------------------------------------------------------- похоже ли на данные

/// Слова текста для сравнения: нижний регистр, ё→е, без знаков препинания, через один пробел.
/// «ООО «Ромашка»» и «ооо ромашка» — одно и то же; «+7 (916) 111-22-33» → «7 916 111 22 33».
pub fn norm_words(s: &str) -> String {
    s.to_lowercase()
        .replace('ё', "е")
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Похоже ли на персональные данные само по себе, без сравнения со значениями: ФИО, номер, email,
/// организация с формой («ООО «Ромашка»», «ИП Петров»). Возвращает, на что похоже, — для пояснения.
/// Обычные заголовки («Ответственный», «Покупатель группа», «Остаток на 01.10.2024») — нет.
pub fn looks_personal(s: &str) -> Option<&'static str> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    if let Some((a, b)) = t.split_once('@') {
        if !a.trim().is_empty() && !a.contains(char::is_whitespace) && b.contains('.') && !b.contains(char::is_whitespace) {
            return Some("похоже на email");
        }
    }
    if looks_number(t) {
        return Some("похоже на номер");
    }
    if looks_company(t) {
        return Some("похоже на организацию");
    }
    if looks_person(t) {
        return Some("похоже на ФИО");
    }
    None
}

/// Номер: пять цифр и больше подряд в одном слове (даты «01.10.2024» и годы «2024-2025» не в счёт)
/// или десять цифр и больше во всём тексте почти без букв (телефон с пробелами).
fn looks_number(t: &str) -> bool {
    let date = |w: &str| {
        let p: Vec<&str> = w.trim_matches(|c: char| !c.is_ascii_digit()).split('.').collect();
        p.len() == 3 && p.iter().all(|x| !x.is_empty() && x.len() <= 4 && x.chars().all(|c| c.is_ascii_digit()))
    };
    let years = |w: &str| {
        let p: Vec<&str> = w.split(['-', '–', '—']).collect();
        p.len() == 2 && p.iter().all(|x| x.len() == 4 && x.chars().all(|c| c.is_ascii_digit()) && (x.starts_with("19") || x.starts_with("20")))
    };
    let long = t.split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | ':' | '/' | '№')).filter(|w| !date(w) && !years(w)).any(|w| {
        let mut run = 0;
        let mut best = 0;
        for c in w.chars() {
            if c.is_ascii_digit() {
                run += 1;
                best = best.max(run);
            } else if !matches!(c, '-' | '(' | ')' | '+' | '.') {
                run = 0;
            }
        }
        best >= 5
    });
    let digits = t.chars().filter(|c| c.is_ascii_digit()).count();
    let letters = t.chars().filter(|c| c.is_alphabetic()).count();
    long || (digits >= 10 && letters < 3)
}

/// Организация с формой собственности: «ООО «Ромашка»», «ИП Петров И.И.», «АО Вектор».
fn looks_company(t: &str) -> bool {
    let words: Vec<&str> = t.split_whitespace().map(|w| w.trim_matches(|c: char| !c.is_alphanumeric())).filter(|w| !w.is_empty()).collect();
    words.len() >= 2 && crate::names::LEGAL_FORMS.contains(&words[0])
}

/// ФИО: два-три слова с большой буквы (или инициалы «И.И.»), и хотя бы одно — известное имя, отчество
/// («…вич», «…вна») или фамилия рядом с инициалами. «Отдел Кадров», «Покупатель группа» — не ФИО.
fn looks_person(t: &str) -> bool {
    let words: Vec<&str> = t.split_whitespace().collect();
    if !(2..=3).contains(&words.len()) {
        return false;
    }
    let initials = |w: &str| {
        let letters: Vec<char> = w.chars().filter(|c| *c != '.').collect();
        w.contains('.') && !letters.is_empty() && letters.len() <= 2 && letters.iter().all(|c| c.is_uppercase())
    };
    let capital = |w: &str| {
        let mut cs = w.chars();
        cs.next().is_some_and(|c| c.is_uppercase() && is_cyrillic(c)) && cs.all(|c| (c.is_alphabetic() && is_cyrillic(c)) || c == '-')
    };
    if !words.iter().all(|w| capital(w) || initials(w)) || words.iter().all(|w| initials(w)) {
        return false;
    }
    let title = |w: &str| {
        let lower = w.to_lowercase().replace('ё', "е");
        let mut cs = lower.chars();
        cs.next().map(|c| c.to_uppercase().chain(cs).collect::<String>()).unwrap_or_default()
    };
    let same = |a: &str, b: &str| a.replace('ё', "е") == b;
    use crate::names::{DETECT_FEMALE, DETECT_MALE, FEMALE, MALE, PATR_STEMS, SURNAMES};
    let name = |w: &str| MALE.iter().chain(FEMALE).chain(DETECT_MALE).chain(DETECT_FEMALE).any(|n| same(n, w));
    let patronymic = |w: &str| {
        let l = w.to_lowercase();
        (l.ends_with("вич") || l.ends_with("вна") || l.ends_with("ична") || l.ends_with("ьич")) && l.chars().count() >= 6
            || PATR_STEMS.iter().any(|s| same(&format!("{s}ич"), w) || same(&format!("{s}на"), w))
    };
    let known_surname = |w: &str| SURNAMES.iter().any(|s| same(s, w) || same(&format!("{s}а"), w));
    let surname = |w: &str| {
        let l = w.to_lowercase();
        known_surname(w) || ["ов", "ев", "ёв", "ин", "ын", "ова", "ева", "ёва", "ина", "ына", "ский", "ская", "цкий", "цкая"].iter().any(|e| l.ends_with(e))
    };
    let full: Vec<String> = words.iter().filter(|w| !initials(w)).map(|w| title(w)).collect();
    let has_initials = words.iter().any(|w| initials(w));
    // Есть имя или отчество; фамилия с инициалами; или все слова — фамилии, и хотя бы одна из справочника.
    full.iter().any(|w| name(w) || patronymic(w))
        || (has_initials && full.iter().any(|w| surname(w)))
        || (full.len() >= 2 && full.iter().any(|w| known_surname(w)) && full.iter().all(|w| surname(w)))
}

fn is_cyrillic(c: char) -> bool {
    matches!(c, 'а'..='я' | 'А'..='Я' | 'ё' | 'Ё')
}

/// Тексты-кандидаты (название колонки, имя листа, подпись шаблона), которые сверяются со значениями под правилами
/// и в ключе. Кандидат совпал, если значение — весь он, или (для ФИО, номеров, организаций, адресов — `partial`)
/// его часть из слов подряд: «Иванов Иван Иванович» в «Менеджер: Иванов Иван Иванович». Части короче четырёх
/// букв не в счёт. Значения «Шаблона» («Опт», «Розница») совпадают только целиком: иначе заголовок «Розница, цена»
/// терял бы название из-за категории в другой колонке.
/// Значений бывают миллионы, кандидатов — десятки: значения не копируются, каждое смотрится один раз.
pub struct Suspects {
    /// Слова кандидата подряд → (номер кандидата, это весь кандидат).
    grams: std::collections::HashMap<String, Vec<(usize, bool)>>,
    /// Где нашлось совпадение (первое) — для пояснения; `None` — не совпал.
    pub found: Vec<Option<String>>,
    longest: usize,
}

impl Suspects {
    pub fn new<'a>(candidates: impl IntoIterator<Item = &'a str>) -> Suspects {
        let mut grams: std::collections::HashMap<String, Vec<(usize, bool)>> = Default::default();
        let mut longest = 0;
        let mut n = 0;
        for (i, c) in candidates.into_iter().enumerate() {
            n = i + 1;
            let norm = norm_words(c);
            longest = longest.max(norm.len());
            let words: Vec<&str> = norm.split(' ').filter(|w| !w.is_empty()).collect();
            // Длинный текст — части не длиннее 16 слов (значения короче), целиком — всегда.
            for a in 0..words.len() {
                for b in a + 1..=words.len().min(a + 16) {
                    let whole = a == 0 && b == words.len();
                    let g = words[a..b].join(" ");
                    if whole || g.chars().filter(|c| *c != ' ').count() >= 4 {
                        let v = grams.entry(g).or_default();
                        if !v.contains(&(i, whole)) {
                            v.push((i, whole));
                        }
                    }
                }
            }
            if words.len() > 16 {
                grams.entry(norm.clone()).or_default().push((i, true));
            }
        }
        Suspects { grams, found: vec![None; n], longest }
    }

    /// Сверяет одно значение; `place` — где оно (записывается при первом совпадении); `partial` — искать и частью
    /// кандидата.
    pub fn check(&mut self, value: &str, place: &str, partial: bool) {
        // Значение длиннее любого кандидата с запасом на знаки препинания — заведомо не его часть.
        if self.grams.is_empty() || value.len() > self.longest * 2 + 64 {
            return;
        }
        if let Some(hits) = self.grams.get(&norm_words(value)) {
            for &(i, whole) in hits {
                if (whole || partial) && self.found[i].is_none() {
                    self.found[i] = Some(place.to_string());
                }
            }
        }
    }

    /// Сверяет значение под правилом типа `kind` (`None` — тип неизвестен, только целиком). Кроме всего значения —
    /// его узнаваемые части, если кандидат — ровно такая часть: фамилия из ФИО (лист «Иванов» при значении
    /// «Иванов Иван Иванович»), название организации без формы (лист «Ромашка» при «ООО «Ромашка»»).
    pub fn check_value(&mut self, value: &str, place: &str, kind: Option<Kind>) {
        self.check(value, place, kind.is_some_and(partial_kind));
        match kind {
            Some(Kind::FullName | Kind::LastName) => {
                for w in value.split_whitespace().filter(|w| w.chars().count() >= 4 && w.starts_with(char::is_uppercase)) {
                    self.check(w, place, false);
                }
            }
            Some(Kind::Company) => {
                let core = crate::fake::split_company(value).1;
                if core.trim() != value.trim() && core.chars().filter(|c| c.is_alphanumeric()).count() >= 4 {
                    self.check(&core, place, false);
                }
            }
            _ => {}
        }
    }
}

/// Значения какого типа ищутся и частью текста (`Suspects::check`): люди, номера, организации, адреса.
pub fn partial_kind(k: Kind) -> bool {
    matches!(
        k,
        Kind::FullName
            | Kind::LastName
            | Kind::FirstName
            | Kind::MiddleName
            | Kind::Email
            | Kind::Phone
            | Kind::Inn
            | Kind::Snils
            | Kind::Passport
            | Kind::Address
            | Kind::Company
            | Kind::Account
    )
}

/// Что в правилах может оказаться персональными данными, по каждому правилу: имя листа и подпись шаблона,
/// совпавшие со значением под правилами (`values`: значение, где оно, тип его правила — см. `Suspects::check_value`)
/// или похожие на данные сами по себе. Строки для журнала — без самих значений (журнал пересылают), только
/// маской «И*** И***». Имена листов не меняются: правила профиля должны находить свои листы.
pub fn rule_risks<'a>(rules: &[Rule], values: impl IntoIterator<Item = (&'a str, &'a str, Option<Kind>)>) -> Vec<Vec<String>> {
    let labels = |r: &Rule| !r.label.trim().is_empty();
    let mut cands: Vec<&str> = Vec::new();
    for r in rules {
        cands.push(r.sheet.as_str());
        if labels(r) {
            cands.push(r.label.trim());
        }
    }
    let mut s = Suspects::new(cands.iter().copied());
    for (v, place, kind) in values {
        s.check_value(v, place, kind);
    }
    let reason = |i: usize, text: &str| -> Option<String> {
        match (&s.found[i], looks_personal(text)) {
            (Some(p), _) => Some(format!("совпадает со значением: {p}")),
            (None, Some(shape)) => Some(shape.to_string()),
            (None, None) => None,
        }
    };
    let mut out = Vec::new();
    let mut i = 0;
    for r in rules {
        let mut mine = Vec::new();
        if let Some(why) = reason(i, &r.sheet) {
            mine.push(format!(
                "в профиле имя листа «{}» похоже на данные ({why}) — перед пересылкой профиля переименуйте лист или не пересылайте профиль; в обезличенном файле имя листа тоже остаётся как есть",
                crate::anon::mask_sample(&r.sheet)
            ));
        }
        i += 1;
        if labels(r) {
            let label = r.label.trim();
            if let Some(why) = reason(i, label) {
                let m = crate::anon::mask_sample(label);
                let tail = if r.kind == Kind::Text {
                    format!("; подпись попадёт и в обезличенный файл («{m} 1», «{m} 2»…) — лучше поменять её")
                } else {
                    String::new()
                };
                mine.push(format!(
                    "в профиле подпись шаблона «{m}» (колонка {}) похожа на данные ({why}) — перед пересылкой профиля поменяйте подпись или не пересылайте профиль{tail}",
                    col_name(r.col)
                ));
            }
            i += 1;
        }
        out.push(mine);
    }
    out
}

/// Предупреждения о профиле перед сохранением и пересылкой (см. `rule_risks`), без повторов: одно имя листа
/// в нескольких правилах — одна строка. Общая проверка окна («Сохранить профиль…», сохранение рядом с файлом)
/// и обработки (журнал, командная строка).
pub fn privacy_risks<'a>(profile: &Profile, values: impl IntoIterator<Item = (&'a str, &'a str, Option<Kind>)>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in rule_risks(&profile.rules, values).into_iter().flatten() {
        if !out.contains(&w) {
            out.push(w);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn personal_shapes() {
        for s in ["Иванов Иван Иванович", "Сидорова Анна", "Петров И.И.", "ИВАНОВ ИВАН", "ООО «Ромашка»", "ИП Петров", "7707083893", "ИНН/КПП: 5029290277", "+7 (916) 111-22-33", "ivanov@mail.ru", "Смирнов Козлов"] {
            assert!(looks_personal(s).is_some(), "{s}");
        }
        for s in ["Ответственный", "Покупатель группа", "Отдел Кадров", "Остаток на 01.10.2024", "Выручка 2024-2025", "Период: 2024 г.", "ФИО Сотрудника", "Сумма, руб.", "Лист1", "Должность", "ООО", "Номер Договора", "Продажи по менеджерам"] {
            assert!(looks_personal(s).is_none(), "{s}");
        }
    }

    #[test]
    fn salt_not_saved_and_ignored_on_load() {
        let json = serde_json::to_string(&Profile::default()).unwrap();
        assert!(!json.contains("salt"), "{json}");
        let old = r#"{"rules": [], "settings": {"id_format": "{fake}", "email_domain": "x.test", "salt": "старая", "clean_metadata": true, "recalc_on_open": true}}"#;
        let p: Profile = serde_json::from_str(old).unwrap();
        assert_ne!(p.settings.salt, "старая");
        assert_eq!(p.settings.salt.len(), 16);
    }
}
