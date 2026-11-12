-- Outreach in each lead's own language. See language.rs.

-- A code from language::LANGUAGES (ISO 639-1, or zh-Hant) set by hand, an import or the API, or '' to go by the lead's country and site.
alter table leads add column language text not null default '';

-- 'lead' (the default) for each lead's own language, or a code from language::LANGUAGES.
alter table segments add column language text not null default 'lead';

-- The language it was written in, which its unsubscribe footer and page use and Analytics groups replies by. '' is English.
alter table email_messages add column language text not null default '';

-- The homepage's <html lang> as a code from language::LANGUAGES, when the tech scan read it and Splee writes in that language, else ''.
alter table tech_scans add column html_lang text not null default '';

-- The project-wide Language setting on Email templates is replaced by each campaign's. A project that had picked a language keeps
-- it in its campaigns. Projects on Auto ('auto' or ''), which wrote in each recipient's country's language, keep the 'lead' default.
-- language.rs's tests run this update straight from this file, so keep it the last statement.
update segments s set language = m.code
from project_settings ps, (values
  ('English', 'en'), ('German', 'de'), ('French', 'fr'), ('Spanish', 'es'), ('Portuguese', 'pt'), ('Italian', 'it'), ('Dutch', 'nl'),
  ('Swedish', 'sv'), ('Norwegian', 'nb'), ('Danish', 'da'), ('Finnish', 'fi'), ('Polish', 'pl'), ('Czech', 'cs'), ('Romanian', 'ro'),
  ('Hungarian', 'hu'), ('Greek', 'el'), ('Turkish', 'tr'), ('Russian', 'ru'), ('Ukrainian', 'uk'), ('Arabic', 'ar'), ('Hebrew', 'he'),
  ('Japanese', 'ja'), ('Korean', 'ko'), ('Chinese (Simplified)', 'zh'), ('Chinese (Traditional)', 'zh-Hant'), ('Slovak', 'sk'),
  ('Bulgarian', 'bg'), ('Hindi', 'hi'), ('Indonesian', 'id'), ('Malay', 'ms'), ('Thai', 'th'), ('Vietnamese', 'vi')
) m(name, code)
where ps.project_id = s.project_id and ps.email_language = m.name;
