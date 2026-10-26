-- Workspace invites: phase 1 of B7 (see docs/architecture.md). The operator
-- creates the auth user from the Supabase dashboard; this table is what
-- lets someone who accepts that invite land in the right workspace with
-- the right role the first time they're seen here, via
-- `crate::auth::provision` / `crate::control::invites::accept_pending_for_email`.

CREATE TABLE workspace_invites (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  workspace_id uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  -- Matched case-insensitively against the verified `email` claim; see
  -- crate::control::invites. Not a profiles FK: the invited person may not
  -- have signed in yet.
  email text NOT NULL,
  role text NOT NULL CHECK (role IN ('owner', 'admin', 'member', 'viewer')),
  invited_by uuid REFERENCES profiles(id) ON DELETE SET NULL,
  expires_at timestamptz NOT NULL,
  accepted_at timestamptz,
  created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX workspace_invites_workspace_idx ON workspace_invites (workspace_id);

-- One pending (unaccepted, unexpired-at-creation-time) invite per
-- (workspace, email) at a time; a new invite is free to be created once an
-- old one is accepted, revoked or has expired.
CREATE UNIQUE INDEX workspace_invites_pending_unique
  ON workspace_invites (workspace_id, lower(email))
  WHERE accepted_at IS NULL;

-- The lookup `crate::auth::provision` does on every authenticated request:
-- this email's still-pending invites, cheaply.
CREATE INDEX workspace_invites_pending_email_idx
  ON workspace_invites (lower(email))
  WHERE accepted_at IS NULL;
