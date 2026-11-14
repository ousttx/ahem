//! Phrases Whisper invents on silence or noise — mostly subtitle credits from its training
//! data. A transcript that is exactly one of them is dropped. Comparison ignores case,
//! punctuation and digits. Short phrases people really say ("Thank you", "you") are not here.

const KNOWN: &[&str] = &[
    "Thank you for watching",
    "Thanks for watching",
    "Thank you for watching, please subscribe",
    "Subtitles by the Amara.org community",
    "Продолжение следует",
    "Субтитры сделал DimaTorzok",
    "Субтитры создавал DimaTorzok",
    "Редактор субтитров А.Семкин Корректор А.Егорова",
    "Sous-titres réalisés par la communauté d'Amara.org",
    "Untertitel im Auftrag des ZDF",
    "Untertitelung des ZDF",
    "Untertitel der Amara.org-Community",
    "Legendas pela comunidade Amara.org",
    "Subtítulos realizados por la comunidad de Amara.org",
    "Subtítulos por la comunidad de Amara.org",
    "Napisy stworzone przez społeczność Amara.org",
    "Napisy wykonane przez społeczność Amara.org",
    "Sottotitoli creati dalla comunità Amara.org",
    "Ondertiteling door de Amara.org gemeenschap",
    "ご視聴ありがとうございました",
    "시청해주셔서 감사합니다",
    "字幕由Amara.org社区提供",
    "请不吝点赞 订阅 转发 打赏支持明镜与点点栏目",
];

pub fn is_hallucination(text: &str) -> bool {
    let text = normalize(text);
    !text.is_empty() && KNOWN.iter().any(|known| normalize(known) == text)
}

fn normalize(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_alphabetic() { c.to_lowercase().next().unwrap_or(c) } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_phrases_are_dropped_in_any_spelling() {
        assert!(is_hallucination("Thank you for watching!"));
        assert!(is_hallucination("  thanks for watching. "));
        assert!(is_hallucination("Субтитры сделал DimaTorzok"));
        assert!(is_hallucination("Продолжение следует..."));
        assert!(is_hallucination("Untertitel im Auftrag des ZDF, 2017"));
        assert!(is_hallucination("ご視聴ありがとうございました。"));
    }

    #[test]
    fn real_dictation_is_kept() {
        assert!(!is_hallucination("Thank you"));
        assert!(!is_hallucination("you"));
        assert!(!is_hallucination("Thank you for watching the kids yesterday"));
        assert!(!is_hallucination("Продолжение следует в понедельник"));
        assert!(!is_hallucination(""));
    }
}
