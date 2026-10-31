// Page length stays within N viewports; long lists are paginated or capped.
import { measureInPage, uiResultOf } from "../lib/ui-context.mjs";
export const MAX_VIEWPORTS = 12;
export const MAX_ROWS = 150;
export const rule = { id: "ui.bounded-length", title: "Page and list length stay bounded", severity: "medium", surface: "ui", targets: { views: "*", widths: [1440, 375] } };
export async function check(ctx) {
  const m = await measureInPage(ctx, (H) => {
    const d = document.documentElement;
    const lists = [];
    for (const list of H.all("tbody, ul, ol, [role='list']")) {
      const rows = list.querySelectorAll(":scope > tr, :scope > li, :scope > [role='listitem']").length;
      if (rows <= 50) continue;
      const scope = list.closest(".panel") ?? list.parentElement ?? list;
      const paginated = Boolean(scope.querySelector("[aria-label*='page' i], .pagination, [class*='pager' i], button[id*='next' i], button[id*='previous' i]"));
      lists.push({ selector: H.selectorOf(list), rows, paginated });
    }
    return { scroll_height: d.scrollHeight, viewport_height: innerHeight, lists };
  });
  const viewports = m.viewport_height > 0 ? m.scroll_height / m.viewport_height : 0;
  const longLists = m.lists.filter((list) => list.rows > MAX_ROWS && !list.paginated);
  const failed = viewports > MAX_VIEWPORTS || longLists.length > 0;
  return uiResultOf(rule, ctx.target, failed ? "fail" : "pass", {
    selector: longLists[0]?.selector ?? (failed ? "viewport" : ""),
    measurements: { scroll_height_px: m.scroll_height, viewport_height_px: m.viewport_height, viewports: Math.round(viewports * 10) / 10, max_viewports: MAX_VIEWPORTS, max_rows: MAX_ROWS, lists: m.lists },
  });
}
