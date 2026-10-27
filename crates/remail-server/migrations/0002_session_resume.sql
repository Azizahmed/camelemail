-- A refresh whose reply never reached the device can be repeated: the
-- session it made is known by `parent_id`, and `access_used_at` says whether
-- the device ever got it (it used the access token).
alter table sessions add column parent_id text;
alter table sessions add column access_used_at timestamptz;
create index sessions_parent on sessions (parent_id);
