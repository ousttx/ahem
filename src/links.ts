// Pages of the project's website that the interface opens in the browser. An address that is
// empty is a page that is not up yet: nothing that leads to it is shown.

/// The project's website. It must be a domain the project owns before a build for users goes
/// out: the app sends people there, and a stranger's page behind these links could ask for money.
export const SITE_URL = "https://useahem.app";

/// The block of the site about supporting the project (the ways to donate live on the site, so
/// they change without a new version of the app). The site opens in the language of the browser.
export const SUPPORT_URL = `${SITE_URL}/#donate`;

/// The block of the site with help and ways to write to the project.
export const HELP_URL = `${SITE_URL}/#help`;

/// Where a link whose page is not up yet points while it is previewed, so the row can be seen
/// and judged.
export const PREVIEW_URL = "https://example.com/support";

/// Turn on to see the links that wait for their pages, pointing at `PREVIEW_URL`. Off in what
/// is committed: a test keeps it so.
export const PREVIEW_LINKS = false;

/// Whether the interface may offer to open `url`: only an https address.
export function openable(url: string): boolean {
  return /^https:\/\/[^\s/]+/.test(url);
}

/// The address a link opens, or "" when the link is not shown: `url` when it is set; while
/// `preview` is on, a link with no page yet shows with `PREVIEW_URL`.
export function linkTarget(url: string, preview: boolean): string {
  if (openable(url)) return url;
  return preview ? PREVIEW_URL : "";
}
