-- The company's own blogs, which social runs draft articles for and crate::blog publishes once approved. A project can
-- connect several. kind is 'wordpress' (url is the site, username and an application password), 'ghost' (url is the
-- admin address, secret_enc the Admin API key id:secret) or 'webflow' (secret_enc an API token, and the site and CMS
-- collection the articles go in, with the collection's fields for the title, body, tags and excerpt). url is the site's
-- public address for Webflow. Secrets are encrypted with APP_SECRET. account is what the weekly cap counts by in any project:
-- the host and path, lowercase, or webflow/{collection_id}. publish false saves articles as drafts on the blog for the user to
-- publish there. paused is set when the blog turns the credentials down, and connecting it again clears it.
create table blogs (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    kind text not null check (kind in ('wordpress', 'ghost', 'webflow')),
    url text not null,
    name text not null default '',
    username text not null default '',
    secret_enc text not null,
    account text not null,
    site_id text not null default '',
    collection_id text not null default '',
    collection_name text not null default '',
    collection_slug text not null default '',
    title_field text not null default '',
    body_field text not null default '',
    tags_field text not null default '',
    excerpt_field text not null default '',
    publish boolean not null default false,
    weekly_cap integer not null default 2 check (weekly_cap between 1 and 7),
    paused boolean not null default false,
    -- The last failure, cleared when an article goes through.
    error text,
    failed_at timestamptz,
    created_at timestamptz not null default now(),
    unique (project_id, account)
);

-- The blog an article goes to, its excerpt, and the slug Splee picks before the first try, so a try that timed out can be
-- found on the blog instead of posted twice. saved_as_draft says the article went in as a draft on the blog.
alter table social_posts
  add column blog_id uuid references blogs(id) on delete set null,
  add column excerpt text not null default '',
  add column slug text not null default '',
  add column saved_as_draft boolean not null default false;

alter table social_posts drop constraint social_posts_platform_check;
alter table social_posts add constraint social_posts_platform_check
  check (platform in ('x', 'reddit', 'bluesky', 'mastodon', 'linkedin', 'threads', 'youtube', 'stackexchange', 'dev', 'discourse', 'lemmy',
                      'nostr', 'tumblr', 'hashnode', 'discord', 'telegram', 'facebook', 'instagram', 'blog'));
