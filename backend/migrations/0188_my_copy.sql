-- My copy: a first email the user writes themselves, sent with each lead's details filled in. See my_copy.rs. The AI email's
-- instructions stay where they were: project_settings.email_instructions and the campaign's angle.
alter table project_settings
  add column my_copy_mode text not null default 'instructions' check (my_copy_mode in ('instructions', 'my_copy')),
  add column my_copy_subject text not null default '',
  add column my_copy_body text not null default '';

-- 'project' sends the project's first email. 'campaign' sends the campaign's own, kept below, so switching back and forth
-- loses neither. my_copy_mode is null until the campaign first gets its own.
alter table segments
  add column my_copy_source text not null default 'project' check (my_copy_source in ('project', 'campaign')),
  add column my_copy_mode text check (my_copy_mode in ('instructions', 'my_copy')),
  add column my_copy_subject text not null default '',
  add column my_copy_body text not null default '';

-- On a first email written while its campaign sent My copy: true for the user's text, false for the AI email one lead in ten
-- gets instead, so the two can be compared. Null otherwise, including a lead that got the AI email for a missing variable.
alter table email_messages add column my_copy boolean;
