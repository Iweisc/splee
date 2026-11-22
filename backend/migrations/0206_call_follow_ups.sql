-- The follow-up email the agent drafts from a call's transcript when someone talked with the lead (agent/calls.rs). It waits
-- for approval like the one after a meeting, and goes out in the lead's thread with the other replies.
alter table calls add column follow_up_email_id uuid references email_messages(id) on delete set null;

alter table email_messages drop constraint email_messages_kind_check;
alter table email_messages add constraint email_messages_kind_check
    check (kind in ('initial', 'follow_up', 'auto_reply', 'manual_reply', 'inbound', 'meeting_follow_up', 'deal_nudge', 'win_back', 'expansion',
                    'referral_ask', 'retention', 'story_ask', 'feature_shipped', 'call_follow_up'));
