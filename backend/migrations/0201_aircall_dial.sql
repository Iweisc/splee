-- Who last put this call step's number in their Aircall Workspace from the Calls page, and when. The call Aircall sends back
-- closes this step when it starts soon after, from the same person to the lead's number.
alter table calls add column aircall_dialed_by uuid references users(id) on delete set null;
alter table calls add column aircall_dialed_at timestamptz;
