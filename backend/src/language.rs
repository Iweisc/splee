//! Outreach in each lead's own language. A campaign writes in each lead's language ("lead", the default) or in one fixed
//! language. A lead's language is picked without Claude, first match wins: the one set on the lead, by hand, an
//! import or the API; its country's, from COUNTRIES; the <html lang> of its company's homepage when the tech scan
//! (tech.rs) has read it; else English. The writer gets the language and how to address people in it as one instruction.

use std::collections::HashMap;
use std::sync::LazyLock;

use sqlx::PgPool;
use uuid::Uuid;

pub struct Language {
    /// ISO 639-1, as stored on leads, campaigns and emails. Traditional Chinese is "zh-Hant", since it shares zh with Simplified.
    pub code: &'static str,
    pub name: &'static str,
    /// How business email in the language addresses a stranger, for the writer.
    pub address: &'static str,
    /// The cold email footer's line before the unsubscribe link.
    pub footer: &'static str,
}

/// The languages a campaign can write in. English first, the rest roughly by how much business email is written in them.
/// Keep in sync with LANGUAGES in frontend/src/lib/languages.ts.
pub const LANGUAGES: [Language; 32] = [
    Language {
        code: "en",
        name: "English",
        address: "Address them by their first name, in a friendly professional register.",
        footer: "Not relevant? Unsubscribe:",
    },
    Language {
        code: "de",
        name: "German",
        address: "Use the formal \"Sie\" throughout. Greet them with \"Guten Tag\" and their full name, like \"Guten Tag Anna Weber,\", \
                  since a first name alone is too familiar from a stranger.",
        footer: "Nicht relevant? Abmelden:",
    },
    Language {
        code: "fr",
        name: "French",
        address: "Use \"vous\" throughout. Greet them with \"Bonjour\" and their first name, like \"Bonjour Camille,\". Follow French \
                  punctuation, with a space before ? ! : and ;.",
        footer: "Pas pertinent ? Se désinscrire :",
    },
    Language {
        code: "es",
        name: "Spanish",
        address: "Use \"usted\". Use \"tú\" only when their company is clearly a startup, an agency or a tech business. Greet them \
                  with \"Hola\" and their first name. Use the Spanish of their country.",
        footer: "¿No le interesa? Darse de baja:",
    },
    Language {
        code: "pt",
        name: "Portuguese",
        address: "Write Brazilian Portuguese with \"você\" for leads in Brazil, and European Portuguese for everyone else, addressing \
                  them politely in the third person. Greet them with \"Olá\" and their first name.",
        footer: "Não é relevante? Cancelar inscrição:",
    },
    Language {
        code: "it",
        name: "Italian",
        address: "Use the formal \"Lei\" throughout. Greet them with \"Buongiorno\" and their first name.",
        footer: "Non è pertinente? Annulla l'iscrizione:",
    },
    Language {
        code: "nl",
        name: "Dutch",
        address: "Use \"u\" for traditional fields such as law, finance, healthcare, government and manufacturing, and \"je\" for \
                  tech, startups, agencies and other creative work. When unsure, use \"u\". Greet them with \"Beste\" and their first name.",
        footer: "Niet relevant? Afmelden:",
    },
    Language {
        code: "sv",
        name: "Swedish",
        address: "Use \"du\", as Swedish business email does, since \"ni\" reads as stiff. Greet them with \"Hej\" and their first name.",
        footer: "Inte relevant? Avregistrera dig:",
    },
    Language {
        code: "nb",
        name: "Norwegian",
        address: "Write Bokmål and use \"du\", as Norwegian business email does. Greet them with \"Hei\" and their first name.",
        footer: "Ikke relevant? Meld deg av:",
    },
    Language {
        code: "da",
        name: "Danish",
        address: "Use \"du\", as Danish business email does. Greet them with \"Hej\" and their first name.",
        footer: "Ikke relevant? Afmeld:",
    },
    Language {
        code: "fi",
        name: "Finnish",
        address: "Use the \"sinä\" forms most Finnish business email uses, in a plain, polite tone. Greet them with \"Hei\" and their first name.",
        footer: "Ei kiinnosta? Peru tilaus:",
    },
    Language {
        code: "pl",
        name: "Polish",
        address: "Use the formal \"Pan\" or \"Pani\" forms, picked by their first name, never \"ty\". Greet them with \"Dzień dobry\".",
        footer: "Nieistotne? Wypisz się:",
    },
    Language {
        code: "cs",
        name: "Czech",
        address: "Use the formal \"vy\" (vykání) throughout. Greet them with \"Dobrý den\".",
        footer: "Nezajímá vás to? Odhlásit se:",
    },
    Language {
        code: "sk",
        name: "Slovak",
        address: "Use the formal \"Vy\" (vykanie) throughout. Greet them with \"Dobrý deň\".",
        footer: "Nemáte záujem? Odhlásiť sa:",
    },
    Language {
        code: "ro",
        name: "Romanian",
        address: "Use the polite \"dumneavoastră\" forms throughout. Greet them with \"Bună ziua\" and their first name.",
        footer: "Nu este relevant? Dezabonare:",
    },
    Language {
        code: "bg",
        name: "Bulgarian",
        address: "Use the formal \"Вие\" throughout, capitalized. Greet them with \"Здравейте\" and \"г-н\" or \"г-жо\" with their \
                  surname, picked by their name, like \"Здравейте, г-жо Иванова,\".",
        footer: "Не е актуално? Отписване:",
    },
    Language {
        code: "hu",
        name: "Hungarian",
        address: "Use the polite \"Ön\" forms throughout. Greet them with \"Kedves\" and their first name.",
        footer: "Nem releváns? Leiratkozás:",
    },
    Language {
        code: "el",
        name: "Greek",
        address: "Use the polite plural forms throughout. Greet them with \"Γεια σας\" and their first name.",
        footer: "Δεν σας ενδιαφέρει; Διαγραφή:",
    },
    Language {
        code: "tr",
        name: "Turkish",
        address: "Use the formal \"siz\" forms throughout. Greet them with \"Merhaba\" and their first name.",
        footer: "İlgilenmiyor musunuz? Abonelikten çık:",
    },
    Language {
        code: "ru",
        name: "Russian",
        address: "Use the formal \"Вы\", capitalized, throughout. Greet them with \"Здравствуйте,\" and their first name.",
        footer: "Неактуально? Отписаться:",
    },
    Language {
        code: "uk",
        name: "Ukrainian",
        address: "Use the formal \"Ви\", capitalized, throughout. Greet them with \"Добрий день,\" and their first name.",
        footer: "Неактуально? Відписатися:",
    },
    Language {
        code: "ar",
        name: "Arabic",
        address: "Write Modern Standard Arabic in a respectful, formal tone. Greet them with \"مرحباً\" and their first name.",
        footer: "غير مهتم؟ إلغاء الاشتراك:",
    },
    Language {
        code: "he",
        name: "Hebrew",
        address: "Hebrew business email is direct and uses first names. Greet them with \"שלום\" and their first name.",
        footer: "לא רלוונטי? להסרה:",
    },
    Language {
        code: "ja",
        name: "Japanese",
        address: "Use polite keigo (desu/masu) throughout. Open with their company name and their family name plus \"様\" on the \
                  first line, like \"株式会社サンプル 山田様\", instead of a first name.",
        footer: "配信停止はこちら:",
    },
    Language {
        code: "ko",
        name: "Korean",
        address: "Use formal polite speech (합니다체) throughout. Address them by their full name plus \"님\" instead of a first name.",
        footer: "관심이 없으신가요? 수신 거부:",
    },
    Language {
        code: "zh",
        name: "Chinese (Simplified)",
        address: "Write Simplified Chinese with \"您\" throughout. Open with their full name and \"您好\" instead of a first name.",
        footer: "不感兴趣？退订：",
    },
    Language {
        code: TRADITIONAL_CHINESE,
        name: "Chinese (Traditional)",
        address: "Write Traditional Chinese with \"您\" throughout, in the wording of Taiwan, or of Hong Kong for leads there. Open with \
                  their full name and \"您好\" instead of a first name.",
        footer: "不感興趣？取消訂閱：",
    },
    Language {
        code: "hi",
        name: "Hindi",
        address: "Write in Devanagari with the respectful \"आप\" throughout, keeping the English business words Indian email uses, \
                  like software or meeting. Greet them with \"नमस्ते\" and their first name plus \"जी\".",
        footer: "रुचि नहीं है? सदस्यता छोड़ें:",
    },
    Language {
        code: "id",
        name: "Indonesian",
        address: "Address them as \"Bapak\" or \"Ibu\", picked by their name, with their first name, like \"Halo Ibu Sari,\", and as \
                  \"Anda\" in the text, never \"kamu\".",
        footer: "Tidak relevan? Berhenti berlangganan:",
    },
    Language {
        code: "ms",
        name: "Malay",
        address: "Write Malaysian Malay, not Indonesian. Address them as \"Encik\" or \"Puan\", picked by their name, with their \
                  first name, like \"Salam sejahtera Puan Aisyah,\", and as \"anda\" in the text.",
        footer: "Tidak berkaitan? Berhenti melanggan:",
    },
    Language {
        code: "th",
        name: "Thai",
        address: "Use the polite register of Thai business email. Open with \"เรียน คุณ\" and their first name, like \"เรียน คุณสมชาย\", and \
                  call them \"คุณ\" plus their first name in the text.",
        footer: "ไม่เกี่ยวข้อง? ยกเลิกการรับอีเมล:",
    },
    Language {
        code: "vi",
        name: "Vietnamese",
        address: "Open with \"Kính gửi\" and \"Anh\" or \"Chị\" with their given name, the last word of a Vietnamese name, like \
                  \"Kính gửi Chị Lan,\". When their name doesn't tell, use \"Quý khách\" with their full name.",
        footer: "Không quan tâm? Hủy đăng ký:",
    },
];

pub const ENGLISH: &Language = &LANGUAGES[0];

/// A campaign setting that writes to each lead in their own language.
pub const EACH_LEAD: &str = "lead";

/// Traditional Chinese's code, the one that isn't ISO 639-1, so parse and html_lang can tell it from Simplified.
pub const TRADITIONAL_CHINESE: &str = "zh-Hant";

pub fn find(code: &str) -> Option<&'static Language> {
    LANGUAGES.iter().find(|l| l.code == code)
}

/// A language as people write it: a code like "de", "de-AT" or "zh-Hant", or its English name like "German". None for "" and
/// for languages Splee doesn't write. Norwegian's "no" and "nn" count as Bokmål, and Chinese for Taiwan, Hong Kong and Macau, or
/// with the Hant script, as Traditional.
pub fn parse(raw: &str) -> Option<&'static Language> {
    let raw = raw.trim().to_lowercase();
    let mut subtags = raw.split(['-', '_']);
    let primary = subtags.next().unwrap_or_default();
    let rest: Vec<&str> = subtags.collect();
    let is_tag = primary.len() == 2 && rest.len() <= 2 && raw.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    let code = match primary {
        "no" | "nn" => "nb",
        "zh" if rest.iter().any(|s| matches!(*s, "hant" | "tw" | "hk" | "mo")) => TRADITIONAL_CHINESE,
        other => other,
    };
    let by_code = is_tag.then(|| find(code)).flatten();
    by_code.or_else(|| LANGUAGES.iter().find(|l| l.name.to_lowercase() == raw || l.name.to_lowercase().starts_with(&format!("{raw} ("))))
}

/// A lead's language from the app or the API: "" clears it, else it has to be one Splee writes.
pub fn lead_setting(raw: &str) -> Result<&'static str, String> {
    if raw.trim().is_empty() {
        return Ok("");
    }
    parse(raw).map(|l| l.code).ok_or_else(|| {
        let codes: Vec<&str> = LANGUAGES.iter().map(|l| l.code).collect();
        format!("language must be empty or one of {}", codes.join(", "))
    })
}

/// The language with this code, English for '' or anything unknown.
pub fn or_english(code: &str) -> &'static Language {
    find(code).unwrap_or(ENGLISH)
}

/// The countries whose businesses write in one language. Countries where business runs in two or more, like Switzerland
/// (German, French, Italian), Belgium (Dutch, French), Canada (English, French), Luxembourg, Cyprus, India, Singapore, the UAE,
/// Lebanon and the Maghreb, are left out on purpose: their leads go by their company's site, else English. That leaves Hindi
/// with no country, since Indian business email is mostly in English. Malaysia and Hong Kong also do business in English, but
/// their leads get Malay and Traditional Chinese, like a project on Auto gave them, unless a lead has its own language set.
const COUNTRIES: &[(&str, &[&str])] = &[
    ("en", &["US", "GB", "IE", "AU", "NZ", "ZA"]),
    ("de", &["DE", "AT", "LI"]),
    ("fr", &["FR", "MC", "SN", "CI", "ML", "BF", "NE", "TG", "BJ", "GA", "CG", "CD", "GN", "MG", "HT"]),
    ("es", &["ES", "MX", "AR", "CO", "CL", "PE", "VE", "EC", "GT", "CU", "BO", "DO", "HN", "PY", "SV", "NI", "CR", "PA", "UY", "GQ"]),
    ("pt", &["PT", "BR", "AO", "MZ", "CV", "GW", "ST"]),
    ("it", &["IT", "SM", "VA"]),
    ("nl", &["NL", "SR"]),
    ("sv", &["SE", "AX"]),
    ("nb", &["NO"]),
    ("da", &["DK"]),
    // Finland has a Swedish-speaking minority, but its business email is in Finnish.
    ("fi", &["FI"]),
    ("pl", &["PL"]),
    ("cs", &["CZ"]),
    ("sk", &["SK"]),
    ("ro", &["RO", "MD"]),
    ("bg", &["BG"]),
    ("hu", &["HU"]),
    ("el", &["GR"]),
    ("tr", &["TR"]),
    ("ru", &["RU", "BY"]),
    ("uk", &["UA"]),
    ("ar", &["SA", "EG", "JO", "IQ", "SY", "YE", "LY", "SD", "KW", "OM", "BH", "QA"]),
    ("he", &["IL"]),
    ("ja", &["JP"]),
    ("ko", &["KR"]),
    ("zh", &["CN"]),
    (TRADITIONAL_CHINESE, &["TW", "HK", "MO"]),
    ("id", &["ID"]),
    ("ms", &["MY", "BN"]),
    ("th", &["TH"]),
    ("vi", &["VN"]),
];

/// A lateral join on `leads l` that adds own.code, the lead's own language, and own.source, where it came from: "set",
/// "country", "site" or "default". The one place the order of the sources lives, so the writer, the Leads filter and the
/// API agree.
pub static OWN_JOIN: LazyLock<String> = LazyLock::new(|| {
    let countries: String = COUNTRIES
        .iter()
        .flat_map(|(code, countries)| countries.iter().map(move |country| format!(" when '{country}' then '{code}'")))
        .collect();
    format!(
        "left join lateral (
           select v.code, v.source from (values
             (1, nullif(l.language, ''), 'set'),
             (2, case upper(l.country_code){countries} end, 'country'),
             (3, (select nullif(t.html_lang, '') from tech_scans t where t.domain = l.company_domain::citext), 'site'),
             (4, 'en', 'default')
           ) v(n, code, source) where v.code is not null order by v.n limit 1
         ) own on true"
    )
});

/// The language each lead's emails are written in: their campaign's, else their own when it says "lead" or they're in no
/// campaign.
pub async fn for_leads(db: &PgPool, ids: &[Uuid]) -> sqlx::Result<HashMap<Uuid, &'static Language>> {
    let rows: Vec<(Uuid, String)> = sqlx::query_as(&format!(
        "select l.id, case when coalesce(s.language, '{EACH_LEAD}') = '{EACH_LEAD}' then own.code else s.language end
         from leads l left join segments s on s.id = l.segment_id {} where l.id = any($1)",
        *OWN_JOIN
    ))
    .bind(ids)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(|(id, code)| (id, or_english(&code))).collect())
}

pub async fn for_lead(db: &PgPool, id: Uuid) -> sqlx::Result<&'static Language> {
    Ok(for_leads(db, &[id]).await?.remove(&id).unwrap_or(ENGLISH))
}

/// The writer's rule for every message to a lead: the language, written natively, and how to address them.
pub fn instruction(language: &Language) -> String {
    let native = if language.code == "en" { String::new() } else { format!(" Write it natively, as a {0} speaker would, not as a translation.", language.name) };
    format!(
        "Language: write in {name}, the subject line too when there is one.{native} {address} Where this differs from the greeting rule \
         above, follow this.",
        name = language.name,
        address = language.address,
    )
}

/// whatlang's name for each language in LANGUAGES. Detecting among these only keeps a short German reply from reading as Estonian.
/// whatlang has no Malay and one Chinese, so those two are missing here. See SAME_TO_WHATLANG.
const WHATLANG: [(whatlang::Lang, &str); 30] = {
    use whatlang::Lang::*;
    [
        (Eng, "en"), (Deu, "de"), (Fra, "fr"), (Spa, "es"), (Por, "pt"), (Ita, "it"), (Nld, "nl"), (Swe, "sv"), (Nob, "nb"), (Dan, "da"), (Fin, "fi"),
        (Pol, "pl"), (Ces, "cs"), (Slk, "sk"), (Ron, "ro"), (Bul, "bg"), (Hun, "hu"), (Ell, "el"), (Tur, "tr"), (Rus, "ru"), (Ukr, "uk"), (Ara, "ar"),
        (Heb, "he"), (Jpn, "ja"), (Kor, "ko"), (Cmn, "zh"), (Hin, "hi"), (Ind, "id"), (Tha, "th"), (Vie, "vi"),
    ]
};

/// Each language whatlang reads as a close one: Malay as Indonesian, and Traditional Chinese as Chinese.
const SAME_TO_WHATLANG: [(&str, &str); 2] = [("ms", "id"), (TRADITIONAL_CHINESE, "zh")];

static DETECTOR: LazyLock<whatlang::Detector> = LazyLock::new(|| whatlang::Detector::with_allowlist(WHATLANG.iter().map(|(lang, _)| *lang).collect()));

/// The language of a reply, read from its text, or None when whatlang isn't sure, which it rarely is about a reply of a few
/// words. The 0.5 is from trying short replies: wrong guesses stayed under 0.25.
fn detect(text: &str) -> Option<&'static Language> {
    let info = DETECTOR.detect(text)?;
    if !info.is_reliable() && info.confidence() < 0.5 {
        return None;
    }
    WHATLANG.iter().find(|(lang, _)| *lang == info.lang()).and_then(|(_, code)| find(code))
}

/// The language to answer a reply in: the one it's written in, or the lead's own when whatlang isn't sure or can't tell the
/// lead's own from the one it found.
pub fn reply_language(text: &str, own: &'static Language) -> &'static Language {
    match detect(text) {
        Some(found) if !SAME_TO_WHATLANG.contains(&(own.code, found.code)) => found,
        _ => own,
    }
}

/// The language of a page's <html lang>, as a code from LANGUAGES, or "" when it has none Splee writes. "de-CH" is German, and
/// Norwegian's "no" and "nn" count as Bokmål.
pub fn html_lang(html: &str) -> &'static str {
    let lower = html.to_ascii_lowercase();
    let Some(start) = lower.find("<html") else { return "" };
    let tag = &lower[start..];
    let tag = &tag[..tag.find('>').unwrap_or(tag.len())];
    // " lang=" so xml:lang and data-lang don't count.
    let Some(at) = tag.find(" lang=").or_else(|| tag.find("\nlang=")) else { return "" };
    let value: String = tag[at + 6..].trim_start_matches(['"', '\'']).chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    parse(&value).map_or("", |l| l.code)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_leads_language_is_theirs_then_their_countrys_then_their_sites_then_english(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Acme') returning id").bind(org).fetch_one(&db).await.unwrap();
        let segment = |language: &'static str| {
            sqlx::query_scalar::<_, Uuid>("insert into segments (project_id, name, language) values ($1, 'C', $2) returning id").bind(project).bind(language)
        };
        let (each, italian) = (segment("lead").fetch_one(&db).await.unwrap(), segment("it").fetch_one(&db).await.unwrap());
        sqlx::query("insert into tech_scans (domain, html_lang) values ('brauhaus.example', 'de'), ('chocolat.example', ''), ('tienda.example', 'es')")
            .execute(&db)
            .await
            .unwrap();
        let lead = |segment: Option<Uuid>, language: &'static str, country: &'static str, domain: Option<&'static str>| {
            sqlx::query_scalar::<_, Uuid>(
                "insert into leads (project_id, segment_id, language, country_code, company_domain) values ($1, $2, $3, $4, $5) returning id",
            )
            .bind(project)
            .bind(segment)
            .bind(language)
            .bind(country)
            .bind(domain)
        };
        let cases = [
            // Set on the lead beats their country.
            (lead(Some(each), "fr", "DE", None), "fr", "set"),
            (lead(Some(each), "", "at", None), "de", "country"),
            // Switzerland and Belgium have more than one language, so they go by the site, else English.
            (lead(Some(each), "", "CH", Some("brauhaus.example")), "de", "site"),
            (lead(Some(each), "", "BE", Some("chocolat.example")), "en", "default"),
            (lead(Some(each), "", "", Some("Tienda.example")), "es", "site"),
            (lead(Some(each), "", "", None), "en", "default"),
        ];
        let mut ids = vec![];
        for (insert, code, source) in cases {
            let id = insert.fetch_one(&db).await.unwrap();
            ids.push((id, code, source));
        }
        let fixed = lead(Some(italian), "", "DE", None).fetch_one(&db).await.unwrap();
        let outside = lead(None, "", "FR", None).fetch_one(&db).await.unwrap();

        let all: Vec<Uuid> = ids.iter().map(|(id, ..)| *id).chain([fixed, outside]).collect();
        let written = for_leads(&db, &all).await.unwrap();
        let rows = crate::routes::leads::lead_rows(&db, &all).await.unwrap();
        for (id, code, source) in ids {
            assert_eq!(written[&id].code, code, "{source}");
            let row = rows.iter().find(|r| r.lead.id == id).unwrap();
            assert_eq!((row.effective.effective_language.as_str(), row.effective.effective_language_source.as_str()), (code, source));
        }
        // A campaign in one language writes everyone in it, and leads outside any campaign get their own.
        assert_eq!((written[&fixed].code, written[&outside].code), ("it", "fr"));
        let own = rows.iter().find(|r| r.lead.id == fixed).unwrap();
        assert_eq!(own.effective.effective_language, "de");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn campaigns_keep_the_language_their_project_wrote_in_before_campaigns_had_one(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        let mut segments = vec![];
        for setting in ["auto", "", "German", "Chinese (Simplified)", "Chinese (Traditional)", "Vietnamese"] {
            let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Acme') returning id").bind(org).fetch_one(&db).await.unwrap();
            sqlx::query("insert into project_settings (project_id, email_language) values ($1, $2)").bind(project).bind(setting).execute(&db).await.unwrap();
            // The column's default, which adding it gave every campaign there was.
            let segment: Uuid =
                sqlx::query_scalar("insert into segments (project_id, name) values ($1, 'C') returning id").bind(project).fetch_one(&db).await.unwrap();
            segments.push(segment);
        }
        let migration = include_str!("../migrations/0136_lead_language.sql");
        sqlx::raw_sql(&migration[migration.find("update segments").unwrap()..]).execute(&db).await.unwrap();
        let mut languages = vec![];
        for segment in segments {
            languages.push(sqlx::query_scalar::<_, String>("select language from segments where id = $1").bind(segment).fetch_one(&db).await.unwrap());
        }
        assert_eq!(languages, ["lead", "lead", "de", "zh", "zh-Hant", "vi"]);
    }

    #[test]
    fn the_migration_maps_every_language_by_its_old_name() {
        // Email templates' old dropdown saved the English name, which the migration's VALUES list turns into a code.
        let migration = include_str!("../migrations/0136_lead_language.sql");
        for l in &LANGUAGES {
            assert!(migration.contains(&format!("('{}', '{}')", l.name, l.code)), "{} is missing from the migration", l.name);
        }
    }

    #[test]
    fn the_frontend_has_every_language_and_its_unsubscribe_page() {
        let frontend = include_str!("../../frontend/src/lib/languages.ts");
        for l in &LANGUAGES {
            assert!(frontend.contains(&format!("{{ code: \"{}\", name: \"{}\" }}", l.code, l.name)), "{} isn't in LANGUAGES", l.code);
            let key = if l.code.contains('-') { format!("\n  \"{}\": {{\n    title: ", l.code) } else { format!("\n  {}: {{\n    title: ", l.code) };
            assert!(frontend.contains(&key), "{} has no unsubscribe page text", l.code);
        }
    }

    #[test]
    fn every_language_has_its_own_code_and_words() {
        for (i, l) in LANGUAGES.iter().enumerate() {
            assert!(LANGUAGES[..i].iter().all(|o| o.code != l.code), "{} twice", l.code);
            assert!(!l.address.is_empty() && l.footer.ends_with([':', '：']), "{}", l.code);
        }
        for (code, _) in COUNTRIES {
            assert!(find(code).is_some(), "{code} isn't a language Splee writes");
        }
        assert_eq!(or_english("xx").code, "en");
        assert_eq!(or_english("").code, "en");
    }

    #[test]
    fn reads_a_language_as_people_write_it() {
        let code = |raw: &str| parse(raw).map(|l| l.code);
        assert_eq!((code("de"), code(" DE-at "), code("German"), code("german")), (Some("de"), Some("de"), Some("de"), Some("de")));
        assert_eq!((code("no"), code("Norwegian"), code("chinese"), code("pt_BR")), (Some("nb"), Some("nb"), Some("zh"), Some("pt")));
        assert_eq!((code(""), code("Klingon"), code("sw"), code("deutsch")), (None, None, None, None));
        // Traditional Chinese is its own language, by its script or by where it's written.
        let chinese = [code("zh-Hant"), code("zh-TW"), code("zh_hk"), code("zh-Hant-HK"), code("Chinese (Traditional)"), code("zh-CN"), code("zh-Hans")];
        assert_eq!(chinese, [Some("zh-Hant"), Some("zh-Hant"), Some("zh-Hant"), Some("zh-Hant"), Some("zh-Hant"), Some("zh"), Some("zh")]);
        assert_eq!((code("ms-MY"), code("Malay"), code("vi"), code("Hindi")), (Some("ms"), Some("ms"), Some("vi"), Some("hi")));
        assert_eq!(lead_setting(" "), Ok(""));
        assert_eq!(lead_setting("French"), Ok("fr"));
        assert!(lead_setting("xx").unwrap_err().contains("de, fr"));
    }

    #[test]
    fn reads_the_homepages_language() {
        assert_eq!(html_lang("<!doctype html><html lang=\"de-CH\" class=\"x\"><head>"), "de");
        assert_eq!(html_lang("<HTML LANG='fr'>"), "fr");
        assert_eq!(html_lang("<html class=\"a\"\nlang=no>"), "nb");
        assert_eq!(html_lang("<html xml:lang=\"de\">"), "");
        assert_eq!(html_lang("<html data-lang=\"de\" lang=\"en-US\">"), "en");
        assert_eq!(html_lang("<html lang=\"sw\">"), "");
        assert_eq!((html_lang("<html lang=\"zh-TW\">"), html_lang("<html lang=\"zh-CN\">"), html_lang("<html lang=\"th\">")), ("zh-Hant", "zh", "th"));
        assert_eq!(html_lang("<body>no html tag</body>"), "");
    }

    #[test]
    fn detects_a_replys_language() {
        let code = |text: &str| detect(text).map(|l| l.code);
        assert_eq!(code("Vielen Dank für Ihre Nachricht. Können Sie mir bitte mehr über die Preise und die Einrichtung erzählen?"), Some("de"));
        assert_eq!(code("Merci pour votre message. Pouvez-vous m'envoyer plus d'informations sur vos tarifs et la mise en place ?"), Some("fr"));
        assert_eq!(code("Gracias por su mensaje. ¿Podría enviarme más información sobre los precios y cómo funciona la instalación?"), Some("es"));
        assert_eq!(code("Obrigado pela mensagem. Você pode me mandar mais informações sobre os preços e como funciona a implantação?"), Some("pt"));
        assert_eq!(code("Hi Alex, thanks. We already have a supplier for this, so not right now."), Some("en"));
        assert_eq!(code("Oui, avec plaisir. Quand êtes-vous disponible ?"), Some("fr"));
        assert_eq!(code("Terima kasih atas pesan Anda. Bisakah Anda mengirimkan informasi lebih lanjut tentang harga dan cara pemasangannya?"), Some("id"));
        assert_eq!(code("Cảm ơn anh đã gửi email. Anh có thể gửi thêm thông tin về giá và cách triển khai được không?"), Some("vi"));
        assert_eq!(code("Ďakujem za vašu správu. Mohli by ste mi poslať viac informácií o cenách a o tom, ako funguje inštalácia?"), Some("sk"));
        // A few words can't tell, so the caller goes by the lead's language rather than whatlang's guess.
        for short in ["ok", "Ja, gerne!", "Sounds good, let's talk next week.", "Merci, pas intéressé."] {
            assert_eq!(code(short), None, "{short}");
        }
    }

    #[test]
    fn a_reply_keeps_the_leads_language_when_whatlang_cant_tell_them_apart() {
        let (malay, traditional) = (find("ms").unwrap(), find(TRADITIONAL_CHINESE).unwrap());
        let malay_reply = "Terima kasih atas e-mel anda. Bolehkah anda menghantar maklumat lanjut mengenai harga dan cara pemasangannya?";
        assert_eq!((reply_language(malay_reply, malay).code, reply_language(malay_reply, ENGLISH).code), ("ms", "id"));
        let chinese_reply = "謝謝您的來信。請問可以再提供更多關於價格和安裝方式的資訊嗎？我們很有興趣了解。";
        assert_eq!((reply_language(chinese_reply, traditional).code, reply_language(chinese_reply, ENGLISH).code), ("zh-Hant", "zh"));
        // A Malay lead who writes in German still gets German, and a short reply keeps theirs.
        let german = "Vielen Dank für Ihre Nachricht. Können Sie mir bitte mehr über die Preise und die Einrichtung erzählen?";
        assert_eq!((reply_language(german, malay).code, reply_language("ok", traditional).code), ("de", "zh-Hant"));
    }

    #[test]
    fn the_instruction_names_the_language_and_how_to_address_them() {
        let de = instruction(find("de").unwrap());
        assert!(de.contains("write in German, the subject line too") && de.contains("not as a translation") && de.contains("\"Sie\""), "{de}");
        let fr = instruction(find("fr").unwrap());
        assert!(fr.contains("write in French") && fr.contains("\"vous\""), "{fr}");
        let nl = instruction(find("nl").unwrap());
        assert!(nl.contains("\"u\"") && nl.contains("\"je\"") && nl.contains("When unsure, use \"u\""), "{nl}");
        let en = instruction(ENGLISH);
        assert!(en.contains("write in English") && !en.contains("translation"), "{en}");
    }
}
