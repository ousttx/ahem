export function buildManifest(input: {
  version: string;
  notes?: string;
  files: Record<string, string>;
  date: string;
}): {
  version: string;
  notes: string;
  pub_date: string;
  platforms: Record<string, { signature: string; url: string }>;
};
