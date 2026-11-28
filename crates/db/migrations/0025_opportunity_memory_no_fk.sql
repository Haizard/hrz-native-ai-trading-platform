-- Opportunity memory rows outlive account deletion: recall is research data,
-- not an account-owned resource. The foreign key prevented users from being
-- cleaned up (including in tests), so it is dropped here. Ownership is still
-- enforced by every query carrying user_id.

ALTER TABLE opportunity_memory DROP CONSTRAINT IF EXISTS opportunity_memory_user_id_fkey;
