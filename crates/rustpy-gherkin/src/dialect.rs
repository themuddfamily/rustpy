//! Gherkin keyword tables, structured like cucumber's `gherkin-languages.json`
//! (the same data behave ships in `behave/i18n.py`). To add a language, add a
//! `static` [`Dialect`] and list it in [`DIALECTS`].

use crate::model::{BlockKind, StepKeyword};

/// Keywords of one Gherkin language.
///
/// Block keywords are followed by `:` in a feature file. Step keywords keep
/// their trailing space (`"Given "`), except keywords ending in an apostrophe
/// (`"Lorsqu'"`) which attach to the next word; `"* "` is listed in every
/// step family, as in the cucumber data.
#[derive(Debug, PartialEq, Eq)]
pub struct Dialect {
    pub code: &'static str,
    pub name: &'static str,
    pub native: &'static str,
    pub feature: &'static [&'static str],
    pub rule: &'static [&'static str],
    pub background: &'static [&'static str],
    pub scenario: &'static [&'static str],
    pub scenario_outline: &'static [&'static str],
    pub examples: &'static [&'static str],
    pub given: &'static [&'static str],
    pub when: &'static [&'static str],
    pub then: &'static [&'static str],
    pub and: &'static [&'static str],
    pub but: &'static [&'static str],
}

pub static EN: Dialect = Dialect {
    code: "en",
    name: "English",
    native: "English",
    feature: &["Feature", "Business Need", "Ability"],
    rule: &["Rule"],
    background: &["Background"],
    scenario: &["Example", "Scenario"],
    scenario_outline: &["Scenario Outline", "Scenario Template"],
    examples: &["Examples", "Scenarios"],
    given: &["* ", "Given "],
    when: &["* ", "When "],
    then: &["* ", "Then "],
    and: &["* ", "And "],
    but: &["* ", "But "],
};

pub static DE: Dialect = Dialect {
    code: "de",
    name: "German",
    native: "Deutsch",
    feature: &["Funktionalität", "Funktion"],
    rule: &["Rule", "Regel"],
    background: &[
        "Grundlage",
        "Hintergrund",
        "Voraussetzungen",
        "Vorbedingungen",
    ],
    scenario: &["Beispiel", "Szenario"],
    scenario_outline: &["Szenariogrundriss", "Szenarien"],
    examples: &["Beispiele"],
    given: &["* ", "Angenommen ", "Gegeben sei ", "Gegeben seien "],
    when: &["* ", "Wenn "],
    then: &["* ", "Dann "],
    and: &["* ", "Und "],
    but: &["* ", "Aber "],
};

pub static FR: Dialect = Dialect {
    code: "fr",
    name: "French",
    native: "français",
    feature: &["Fonctionnalité"],
    rule: &["Règle"],
    background: &["Contexte"],
    scenario: &["Exemple", "Scénario"],
    scenario_outline: &["Plan du scénario", "Plan du Scénario"],
    examples: &["Exemples"],
    given: &[
        "* ",
        "Soit ",
        "Sachant que ",
        "Sachant qu'",
        "Sachant ",
        "Etant donné que ",
        "Etant donné qu'",
        "Etant donné ",
        "Etant donnée ",
        "Etant donnés ",
        "Etant données ",
        "Étant donné que ",
        "Étant donné qu'",
        "Étant donné ",
        "Étant donnée ",
        "Étant donnés ",
        "Étant données ",
    ],
    when: &["* ", "Quand ", "Lorsque ", "Lorsqu'"],
    then: &["* ", "Alors ", "Donc "],
    and: &["* ", "Et que ", "Et qu'", "Et "],
    but: &["* ", "Mais que ", "Mais qu'", "Mais "],
};

pub static ES: Dialect = Dialect {
    code: "es",
    name: "Spanish",
    native: "español",
    feature: &["Característica", "Necesidad del negocio", "Requisito"],
    rule: &["Regla", "Regla de negocio"],
    background: &["Antecedentes"],
    scenario: &["Ejemplo", "Escenario"],
    scenario_outline: &["Esquema del escenario"],
    examples: &["Ejemplos"],
    given: &["* ", "Dado ", "Dada ", "Dados ", "Dadas "],
    when: &["* ", "Cuando "],
    then: &["* ", "Entonces "],
    and: &["* ", "Y ", "E "],
    but: &["* ", "Pero "],
};

pub static IT: Dialect = Dialect {
    code: "it",
    name: "Italian",
    native: "italiano",
    feature: &["Funzionalità", "Esigenza di Business", "Abilità"],
    rule: &["Regola"],
    background: &["Contesto"],
    scenario: &["Esempio", "Scenario"],
    scenario_outline: &["Schema dello scenario"],
    examples: &["Esempi"],
    given: &["* ", "Dato ", "Data ", "Dati ", "Date "],
    when: &["* ", "Quando "],
    then: &["* ", "Allora "],
    and: &["* ", "E ", "Ed "],
    but: &["* ", "Ma "],
};

pub static NL: Dialect = Dialect {
    code: "nl",
    name: "Dutch",
    native: "Nederlands",
    feature: &["Functionaliteit"],
    rule: &["Regel"],
    background: &["Achtergrond"],
    scenario: &["Voorbeeld", "Scenario"],
    scenario_outline: &["Abstract Scenario"],
    examples: &["Voorbeelden"],
    given: &["* ", "Gegeven ", "Stel "],
    when: &["* ", "Als ", "Wanneer "],
    then: &["* ", "Dan "],
    and: &["* ", "En "],
    but: &["* ", "Maar "],
};

pub static PT: Dialect = Dialect {
    code: "pt",
    name: "Portuguese",
    native: "português",
    feature: &["Funcionalidade", "Característica", "Caracteristica"],
    rule: &["Regra"],
    background: &["Contexto", "Cenário de Fundo", "Cenario de Fundo", "Fundo"],
    scenario: &["Exemplo", "Cenário", "Cenario"],
    scenario_outline: &[
        "Esquema do Cenário",
        "Esquema do Cenario",
        "Delineação do Cenário",
        "Delineacao do Cenario",
    ],
    examples: &["Exemplos", "Cenários", "Cenarios"],
    given: &["* ", "Dado ", "Dada ", "Dados ", "Dadas "],
    when: &["* ", "Quando "],
    then: &["* ", "Então ", "Entao "],
    and: &["* ", "E "],
    but: &["* ", "Mas "],
};

pub static RU: Dialect = Dialect {
    code: "ru",
    name: "Russian",
    native: "русский",
    feature: &[
        "Функция",
        "Функциональность",
        "Функционал",
        "Свойство",
        "Фича",
    ],
    rule: &["Правило"],
    background: &["Предыстория", "Контекст"],
    scenario: &["Пример", "Сценарий"],
    scenario_outline: &["Структура сценария", "Шаблон сценария"],
    examples: &["Примеры"],
    given: &["* ", "Допустим ", "Дано ", "Пусть "],
    when: &["* ", "Когда ", "Если "],
    then: &["* ", "То ", "Затем ", "Тогда "],
    and: &["* ", "И ", "К тому же ", "Также "],
    but: &["* ", "Но ", "А ", "Иначе "],
};

/// Every bundled language; the first one is the default.
pub static DIALECTS: &[&Dialect] = &[&EN, &DE, &FR, &ES, &IT, &NL, &PT, &RU];

/// The dialect for a language code (`"de"`, case-insensitive; `"de-CH"` falls
/// back to `"de"`).
pub fn dialect(code: &str) -> Option<&'static Dialect> {
    let code = code.trim();
    let exact = DIALECTS
        .iter()
        .copied()
        .find(|d| d.code.eq_ignore_ascii_case(code));
    exact.or_else(|| {
        let base = code.split(['-', '_']).next().unwrap_or(code);
        DIALECTS
            .iter()
            .copied()
            .find(|d| d.code.eq_ignore_ascii_case(base))
    })
}

/// English.
pub fn default_dialect() -> &'static Dialect {
    &EN
}

/// Case-insensitive (Unicode lower-case) prefix test; returns the byte
/// length of the matched prefix of `text`.
fn prefix_len(text: &str, prefix: &str, ignore_case: bool) -> Option<usize> {
    if !ignore_case {
        return text.starts_with(prefix).then_some(prefix.len());
    }
    let mut chars = text.char_indices();
    for p in prefix.chars() {
        let (_, t) = chars.next()?;
        if t != p && !t.to_lowercase().eq(p.to_lowercase()) {
            return None;
        }
    }
    Some(chars.next().map(|(i, _)| i).unwrap_or(text.len()))
}

impl Dialect {
    /// Keywords of a block family.
    pub fn block_keywords(&self, kind: BlockKind) -> &'static [&'static str] {
        match kind {
            BlockKind::Feature => self.feature,
            BlockKind::Rule => self.rule,
            BlockKind::Background => self.background,
            BlockKind::Scenario => self.scenario,
            BlockKind::ScenarioOutline => self.scenario_outline,
            BlockKind::Examples => self.examples,
        }
    }

    /// Step keywords of a family (with their trailing space).
    pub fn step_keywords(&self, kind: StepKeyword) -> &'static [&'static str] {
        match kind {
            StepKeyword::Given => self.given,
            StepKeyword::When => self.when,
            StepKeyword::Then => self.then,
            StepKeyword::And => self.and,
            StepKeyword::But => self.but,
            StepKeyword::Star => &["* "],
        }
    }

    /// Every step keyword (deduplicated, trailing spaces trimmed except for
    /// apostrophe keywords), for completion lists.
    pub fn all_step_keywords(&self) -> Vec<(StepKeyword, &'static str)> {
        let mut out: Vec<(StepKeyword, &'static str)> = Vec::new();
        for kind in [
            StepKeyword::Given,
            StepKeyword::When,
            StepKeyword::Then,
            StepKeyword::And,
            StepKeyword::But,
        ] {
            for kw in self.step_keywords(kind) {
                let k = kw.trim_end();
                if k == "*" {
                    continue;
                }
                if !out.iter().any(|(_, o)| *o == k) {
                    out.push((kind, k));
                }
            }
        }
        out.push((StepKeyword::Star, "*"));
        out
    }

    /// Matches a block keyword followed by `:` at the start of `line` (which
    /// must already be left-trimmed). Returns the family, the keyword and the
    /// byte length of `keyword:`.
    pub fn match_block(&self, line: &str) -> Option<(BlockKind, &'static str, usize)> {
        let kinds = [
            BlockKind::Feature,
            BlockKind::Rule,
            BlockKind::Background,
            BlockKind::ScenarioOutline,
            BlockKind::Scenario,
            BlockKind::Examples,
        ];
        let mut best: Option<(BlockKind, &'static str, usize)> = None;
        for kind in kinds {
            for kw in self.block_keywords(kind) {
                if line.starts_with(kw)
                    && line[kw.len()..].starts_with(':')
                    && best.is_none_or(|(_, b, _)| kw.len() > b.len())
                {
                    best = Some((kind, kw, kw.len() + 1));
                }
            }
        }
        best
    }

    /// Matches a step keyword at the start of `line` (left-trimmed). Exact
    /// case first, then case-insensitively (behave accepts `given ...`).
    /// Returns the keyword family and the byte length of the keyword as it
    /// appears in `line` (without the separating whitespace).
    pub fn match_step(&self, line: &str) -> Option<(StepKeyword, usize)> {
        let mut candidates: Vec<(StepKeyword, &'static str)> = Vec::new();
        for kind in [
            StepKeyword::Given,
            StepKeyword::When,
            StepKeyword::Then,
            StepKeyword::And,
            StepKeyword::But,
        ] {
            for kw in self.step_keywords(kind) {
                let family = if kw.trim_end() == "*" {
                    StepKeyword::Star
                } else {
                    kind
                };
                if !candidates.iter().any(|(_, c)| c == kw) {
                    candidates.push((family, kw));
                }
            }
        }
        candidates.sort_by_key(|(_, kw)| std::cmp::Reverse(kw.chars().count()));
        for ignore_case in [false, true] {
            for (family, kw) in &candidates {
                let base = kw.trim_end();
                let Some(len) = prefix_len(line, base, ignore_case) else {
                    continue;
                };
                let rest = &line[len..];
                // "Given" must be followed by whitespace (or end the line);
                // apostrophe keywords ("Lorsqu'") glue to the next word.
                let glued = !kw.ends_with(' ');
                if glued || rest.is_empty() || rest.starts_with(char::is_whitespace) {
                    return Some((*family, len));
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_by_code() {
        assert_eq!(dialect("de").map(|d| d.name), Some("German"));
        assert_eq!(dialect("FR").map(|d| d.code), Some("fr"));
        assert_eq!(dialect("de-CH").map(|d| d.code), Some("de"));
        assert!(dialect("tlh").is_none());
        assert_eq!(default_dialect().code, "en");
        assert_eq!(DIALECTS[0].code, "en");
    }

    #[test]
    fn block_keywords_need_a_colon_and_prefer_the_longest() {
        assert_eq!(
            EN.match_block("Scenario Outline: x"),
            Some((BlockKind::ScenarioOutline, "Scenario Outline", 17))
        );
        assert_eq!(
            EN.match_block("Scenario: x"),
            Some((BlockKind::Scenario, "Scenario", 9))
        );
        assert_eq!(
            EN.match_block("Scenarios:"),
            Some((BlockKind::Examples, "Scenarios", 10))
        );
        assert_eq!(EN.match_block("Scenario x"), None);
        assert_eq!(
            EN.match_block("Business Need: y").map(|b| b.0),
            Some(BlockKind::Feature)
        );
        assert_eq!(
            DE.match_block("Funktionalität: Rechner").map(|b| b.2),
            Some("Funktionalität:".len())
        );
        assert_eq!(
            ES.match_block("Regla de negocio: r").map(|b| b.1),
            Some("Regla de negocio")
        );
        assert_eq!(
            RU.match_block("Примеры:").map(|b| b.0),
            Some(BlockKind::Examples)
        );
        assert_eq!(
            RU.match_block("Пример:").map(|b| b.0),
            Some(BlockKind::Scenario)
        );
    }

    #[test]
    fn step_keywords() {
        assert_eq!(EN.match_step("Given I have"), Some((StepKeyword::Given, 5)));
        assert_eq!(EN.match_step("And more"), Some((StepKeyword::And, 3)));
        assert_eq!(EN.match_step("* bullet"), Some((StepKeyword::Star, 1)));
        assert_eq!(EN.match_step("Given"), Some((StepKeyword::Given, 5)));
        assert_eq!(EN.match_step("Given\tx"), Some((StepKeyword::Given, 5)));
        assert_eq!(EN.match_step("Givenx"), None);
        assert_eq!(
            EN.match_step("given lower case"),
            Some((StepKeyword::Given, 5))
        );
        assert_eq!(EN.match_step("Gherkin"), None);
        // Longest keyword wins; apostrophe keywords glue to the next word.
        assert_eq!(
            FR.match_step("Etant donné que x"),
            Some((StepKeyword::Given, "Etant donné que".len()))
        );
        assert_eq!(
            FR.match_step("Etant donné x"),
            Some((StepKeyword::Given, "Etant donné".len()))
        );
        assert_eq!(
            FR.match_step("Lorsqu'il pleut"),
            Some((StepKeyword::When, "Lorsqu'".len()))
        );
        assert_eq!(
            FR.match_step("Et qu'il"),
            Some((StepKeyword::And, "Et qu'".len()))
        );
        assert_eq!(
            DE.match_step("Gegeben seien zwei"),
            Some((StepKeyword::Given, "Gegeben seien".len()))
        );
        assert_eq!(DE.match_step("Und dann"), Some((StepKeyword::And, 3)));
        assert_eq!(
            RU.match_step("Допустим я"),
            Some((StepKeyword::Given, "Допустим".len()))
        );
        // Case-insensitive matching of non-ASCII keywords.
        assert_eq!(
            FR.match_step("étant donné x"),
            Some((StepKeyword::Given, "étant donné".len()))
        );
    }

    #[test]
    fn completion_keywords() {
        let kws = EN.all_step_keywords();
        assert_eq!(
            kws,
            vec![
                (StepKeyword::Given, "Given"),
                (StepKeyword::When, "When"),
                (StepKeyword::Then, "Then"),
                (StepKeyword::And, "And"),
                (StepKeyword::But, "But"),
                (StepKeyword::Star, "*")
            ]
        );
        assert!(FR
            .all_step_keywords()
            .contains(&(StepKeyword::When, "Lorsqu'")));
    }
}
