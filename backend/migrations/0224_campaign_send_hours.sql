-- A campaign's own send days and hours for its first emails and follow-ups, read in the project's time zone, or each lead's with
-- lead_time_zone, and kept off the project's days off. All three null follows the project's send window (mail::sender).
alter table segments
    add column send_days int[],
    add column send_start_hour int,
    add column send_end_hour int,
    add constraint segments_send_hours check (
        (send_days is null and send_start_hour is null and send_end_hour is null)
        or (send_days is not null and send_start_hour is not null and send_end_hour is not null and send_start_hour between 0 and 23 and send_end_hour between 1 and 24 and send_start_hour < send_end_hour)
    );
