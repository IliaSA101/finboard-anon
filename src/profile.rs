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
}

impl Kind {
    pub const ALL: [Kind; 16] = [
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
            Kind::Text => "Текст + ID",
            Kind::Clear => "Очистить",
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
            Kind::Text => "<Подпись> <ID>, например «Клиент 12»",
            Kind::Clear => "Пустое значение",
        }
    }

    /// Можно ли включать/выключать ID для этого типа.
    pub fn id_optional(self) -> bool {
        !matches!(self, Kind::BirthDate | Kind::Mask | Kind::Text | Kind::Clear)
    }

    pub fn default_with_id(self) -> bool {
        !matches!(
            self,
            Kind::LastName | Kind::FirstName | Kind::MiddleName | Kind::BirthDate | Kind::Mask | Kind::Clear
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
            Kind::BirthDate | Kind::Mask | Kind::Clear => return None,
        };
        Some(ns.to_string())
    }

    pub fn from_title(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.title() == s)
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
    /// Подпись для типа «Текст + ID».
    #[serde(default)]
    pub label: String,
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
        }
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

    pub fn with_id_effective(&self) -> bool {
        match self.kind {
            Kind::Text => true,
            k if k.id_optional() => self.with_id,
            _ => false,
        }
    }

    /// Ключ типа замены: одно и то же значение под одним ключом всегда даёт один фейк.
    /// Пишется в колонку «Тип» файла-ключа.
    pub fn kind_key(&self) -> String {
        let mut s = self.kind.title().to_string();
        if self.kind.id_optional() && self.with_id {
            s.push_str("+ID");
        }
        if self.kind == Kind::Text {
            s.push_str(&format!("[{}]", self.label_or_default()));
        }
        s
    }
}

/// Разбирает ключ типа обратно: (тип, с ID, подпись).
pub fn parse_kind_key(key: &str) -> Option<(Kind, bool, String)> {
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
    #[serde(default = "yes")]
    pub inner_text: bool,
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

#[cfg(test)]
mod tests {
    use super::*;

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
