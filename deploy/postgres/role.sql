-- Run as a PostgreSQL administrator after Platform's `01-database-and-roles.sql` created the shared
-- database `ratatoskr`, and before the first `ratatoskr-x` start. Set the password separately with
-- `\password ratatoskr_x`; no credential belongs in this repository. The service creates its own
-- `x_archive` schema from `schema.sql` at start, which is why the role needs `create` on the
-- database and nothing more.

do $$
begin
    if not exists (select 1 from pg_roles where rolname = 'ratatoskr_x') then
        create role ratatoskr_x login;
    end if;
end
$$;

alter role ratatoskr_x nosuperuser nocreatedb nocreaterole noreplication nobypassrls;
grant connect, create on database ratatoskr to ratatoskr_x;

\connect ratatoskr
revoke all on schema public from ratatoskr_x;
