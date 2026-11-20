export type Artifact = {
  artifact_ref: string;
  title: string;
  kind?: string | null;
  format?: string | null;
  target_kind?: string | null;
  slug?: string | null;
  updated_at_ms?: number | null;
  pinned: boolean;
  can_open: boolean;
  can_share: boolean;
};

export type Snapshot = { artifacts: Artifact[]; total: number };
