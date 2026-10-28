// Held-response regression scenarios for the shipped Settings modules. The
// containing browser fixture supplies the real dashboard document and CSS;
// this page omits app.js so no background poll can race request assertions.
import assert from 'node:assert/strict';
import path from 'node:path';

export async function checkSettingsMutationOwnership(browser, origin, evidence = null) {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  try {
    await page.route('**/settings-mutations', async (route) => route.fulfill({
      contentType: 'text/html',
      body: '<!doctype html><link rel="stylesheet" href="/static/dashboard.css"><main id="config-body"></main><div id="config-controls"></div><span id="config-count"></span><p id="config-explainer"></p>',
    }));
    await page.goto(`${origin}/settings-mutations`);
    const results = await page.evaluate(async () => {
      const common = await import('/static/js/common.js');
      const topics = await import('/static/js/config-topics.js');
      const config = await import('/static/js/config.js');
      const body = document.getElementById('config-body');
      const row = (key, value, value_type = 'string') => ({
        key, label: key, value, value_type, state: 'set', settable: true,
        source: { layer: 'workspace' }, equals: { built_in: true },
      });
      const payload = {
        config_set: { authorized: true },
        sections: [{ token: 'workflow', keys: [
          row('workflow.base_branch', 'agent-main'), row('pr.draft', true, 'bool'),
          row('workflow.resource_throttle.enabled', true, 'bool'),
        ] }],
        crews: [{ name: 'fixture', provider: 'codex', model: 'fixture', source: 'workspace' }],
        crew_fields: ['model'],
      };
      const writes = [];
      let reads = 0;
      let pending = null;
      let reloads = 0;
      let redraws = 0;
      let hold = false;
      let refuseKey = null;
      window.fetch = async (url, options = {}) => {
        if (['PUT', 'DELETE'].includes(options.method)) {
          writes.push({ url, method: options.method, body: options.body && JSON.parse(options.body) });
          if (hold) {
            hold = false;
            return new Promise((resolve) => { pending = resolve; });
          }
          if (refuseKey && url.includes(refuseKey)) return new Response(JSON.stringify({ error: 'fixture refusal' }), { status: 403 });
          return new Response('{}');
        }
        reads++;
        return new Response(JSON.stringify(payload));
      };
      // Responses and immediate refreshes drain their microtasks before this
      // frame. No sleep, polling deadline or concurrent workload orders a race.
      const settled = () => new Promise(requestAnimationFrame);
      const release = async (failure) => {
        if (!pending) throw new Error('expected a held Settings request');
        const resolve = pending; pending = null;
        resolve(new Response(JSON.stringify(failure ? { error: 'retired refusal' } : {}), { status: failure ? 403 : 200 }));
        await settled();
      };
      const check = (condition, message) => { if (!condition) throw new Error(message); };
      const click = (selector, label) => {
        const button = [...body.querySelectorAll(selector)].find((node) => node.textContent === label);
        if (!button) throw new Error(`missing Settings control ${label}`);
        button.click();
      };
      common.setWorkspace('ws-a');
      config.initConfig();
      common.onWorkspaceChange(() => topics.resetTopics());
      const context = {
        canEdit: true,
        rerender: () => { redraws++; body.replaceChildren(); topics.renderAllKeys(body, payload, context); },
        reload: async () => { reloads++; context.rerender(); },
      };
      const reset = () => {
        topics.resetTopics(); writes.length = 0; reads = 0; reloads = 0; redraws = 0;
        common.setHost(null); common.setWorkspace('ws-reset'); common.setWorkspace('ws-a'); context.rerender();
      };
      const openDrawer = (key = 'workflow.base_branch') => body.querySelector(`[data-key="${key}"] button`).click();
      const results = [];
      for (const transition of ['host', 'workspace-round-trip']) {
        for (const failure of [false, true]) {
          reset(); hold = true;
          click('button', 'Remove all 3 workspace values');
          check(writes.length === 1, 'bulk DELETE starts sequentially');
          if (transition === 'host') common.setHost('remote');
          else { common.setWorkspace('ws-b'); common.setWorkspace('ws-a'); }
          context.rerender();
          const draws = redraws;
          await release(failure);
          check(writes.length === 1, `${transition}: retired bulk deletion sent a subsequent DELETE`);
          check(reloads === 0 && redraws === draws && !body.textContent.includes('Removed '), `${transition}: retired bulk feedback or refresh`);
          results.push(`bulk-${transition}-${failure ? 'refusal' : 'success'}-retires`);
        }
      }
      reset(); hold = true;
      click('button', 'Remove all 3 workspace values');
      const retired = pending;
      topics.resetTopics(); context.rerender(); hold = true;
      click('button', 'Remove all 3 workspace values');
      retired(new Response('{}')); await settled();
      check(writes.length === 2 && reloads === 0, 'reset retires a bulk operation within the same visit');
      check([...body.querySelectorAll('button')].find((button) => button.textContent === 'Remove all 3 workspace values').disabled, 'retired completion cannot release the new bulk operation');
      await release(false);
      check(writes.length === 4 && reloads === 1, 'the replacement bulk operation finishes its own keys');
      results.push('bulk-reset-preserves-new-operation-ownership');
      reset();
      refuseKey = 'pr.draft';
      click('button', 'Remove all 3 workspace values');
      await settled();
      check(writes.length === 3 && writes.every((entry) => entry.url.endsWith('scope=workspace&workspace=ws-a')), 'same-visit bulk requests retain their workspace');
      check(body.textContent.includes('Removed 2 workspace value(s). Failed: pr.draft: fixture refusal'), 'same-visit successes and per-key refusal are reported');
      refuseKey = null;
      results.push('bulk-same-visit-sequential-successes-and-per-key-refusal');

      for (const action of ['Save', 'Remove workspace value']) {
        for (const transition of ['replacement', 'host', 'workspace-round-trip']) {
          for (const failure of [false, true]) {
            reset(); openDrawer(); hold = true;
            click('.drawer-actions button', action);
            const first = writes[0];
            check(first?.url.endsWith('workspace=ws-a'), 'drawer request pins the initiating workspace');
            if (transition === 'replacement') click('.drawer-actions button', 'Close');
            else if (transition === 'host') common.setHost('remote');
            else { common.setWorkspace('ws-b'); common.setWorkspace('ws-a'); }
            context.rerender(); openDrawer('pr.draft');
            const draws = redraws;
            await release(failure);
            check(reloads === 0 && redraws === draws, `${action}/${transition}: retired drawer refreshed the new session`);
            check(body.querySelector('.config-drawer .config-key')?.textContent === 'pr.draft', 'replacement drawer stays open');
            check(!body.textContent.includes('retired refusal'), 'replacement drawer inherits no old refusal');
            results.push(`drawer-${action}-${transition}-${failure ? 'refusal' : 'success'}-retires`);
          }
        }
        reset(); openDrawer(); hold = true;
        click('.drawer-actions button', action);
        check([...body.querySelectorAll('.drawer-actions button')].filter((button) => button.textContent !== 'Close').every((button) => button.disabled), 'both drawer mutations are disabled during a write');
        await release(true);
        check(body.textContent.includes('retired refusal'), 'current drawer retains its refusal and draft');
        click('.drawer-actions button', action);
        await settled();
        check(reloads === 1 && !body.querySelector('.config-drawer'), 'current drawer success reloads and closes');
      }

      for (const kind of ['key-save', 'key-clear', 'crew-save', 'crew-delete']) {
        for (const transition of ['replacement', 'host', 'workspace-round-trip']) {
          for (const failure of [false, true]) {
            reset();
            const crew = kind.startsWith('crew');
            config.setConfigSubtab(crew ? 'crews' : 'machine');
            await config.fetchAndRenderConfig();
            const pencil = () => {
              const button = body.querySelector(crew ? '.config-crew-row .config-pencil' : '.config-sys-enabled .config-pencil');
              check(button, `${kind}/${transition}/${failure}: missing edit button: ${body.textContent}`);
              return button;
            };
            pencil().click(); hold = true;
            click('.config-editor button', kind.endsWith('save') ? 'Save' : kind === 'key-clear' ? 'Clear' : 'Delete');
            check(writes.length === 1 && writes[0].url.endsWith('workspace=ws-a'), `${kind}: request pins the initiating workspace`);
            check(writes[0].method === (kind.endsWith('save') ? 'PUT' : 'DELETE'), `${kind}: correct mutation`);
            if (transition === 'replacement') {
              // Opening another editor is allowed while the prior write waits.
              if (crew) click('button', '+ Add crew');
              else {
                config.setConfigSubtab('workflow'); config.setConfigSubtab('machine');
                await config.fetchAndRenderConfig(); pencil().click();
              }
            } else {
              if (transition === 'host') common.setHost('remote');
              else { common.setWorkspace('ws-b'); common.setWorkspace('ws-a'); }
              await config.fetchAndRenderConfig(); pencil().click();
            }
            const readCount = reads;
            const editor = body.querySelector('.config-editor');
            await release(failure);
            check(reads === readCount, `${kind}/${transition}: retired mutation reloaded the current Settings view`);
            check(body.querySelector('.config-editor') === editor && !body.textContent.includes('retired refusal'), `${kind}: retired result changed the replacement editor`);
            results.push(`${kind}-${transition}-${failure ? 'refusal' : 'success'}-retires`);
          }
        }
        // The same caller still publishes current failures and successful writes.
        reset();
        const crew = kind.startsWith('crew');
        config.setConfigSubtab(crew ? 'crews' : 'machine'); await config.fetchAndRenderConfig();
        body.querySelector(crew ? '.config-crew-row .config-pencil' : '.config-sys-enabled .config-pencil').click();
        hold = true;
        const action = kind.endsWith('save') ? 'Save' : kind === 'key-clear' ? 'Clear' : 'Delete';
        click('.config-editor button', action); await release(true);
        check(body.textContent.includes('retired refusal'), `${kind}: current refusal is visible`);
        click('.config-editor button', action); await settled();
        check(!body.querySelector('.config-editor'), `${kind}: current success closes the editor after refresh`);
      }
      // A plugin-only file exists, but is not a workspace policy. Both
      // editor families must let the operator make the explicit first choice.
      payload.workspace_file_exists = true;
      payload.layers = {
        global: { path: '/global/config.toml' }, workspace: { path: '/workspace/.orbit/config.toml' },
        execution_not_inherited: false,
      };
      for (const init of ['seed-from-global', 'fresh']) {
        reset();
        config.setConfigSubtab('all-keys'); await config.fetchAndRenderConfig();
        openDrawer();
        click('.drawer-actions button', init === 'fresh' ? 'Start empty' : 'Seed from global');
        await settled();
        check(writes.length === 1 && writes[0].body.init === init, `plugin-only key editor sends ${init}`);
        reset();
        config.setConfigSubtab('crews'); await config.fetchAndRenderConfig();
        click('button', '+ Add crew');
        const name = body.querySelector('.config-editor input');
        name.value = 'first-policy'; name.dispatchEvent(new Event('input', { bubbles: true }));
        hold = true;
        click('.config-editor button', 'Save'); await release(true);
        check(body.querySelector('.config-editor'), 'refused first crew save keeps the draft');
        click('.config-editor button', init === 'fresh' ? 'Start empty' : 'Copy global policy');
        await settled();
        check(writes.length === 2 && writes[1].body.init === init, `plugin-only crew editor sends ${init}`);
        results.push(`plugin-only-workspace-key-and-crew-${init}`);
      }
      // Leave the refusal and its initialization choices visible for review.
      reset(); config.setConfigSubtab('crews'); await config.fetchAndRenderConfig();
      click('button', '+ Add crew');
      const name = body.querySelector('.config-editor input');
      name.value = 'first-policy'; name.dispatchEvent(new Event('input', { bubbles: true }));
      hold = true; click('.config-editor button', 'Save'); await release(true);
      return results;
    });
    assert.ok(results.length > 0);
    if (evidence) for (const width of [1440, 375]) {
      await page.setViewportSize({ width, height: 900 });
      await page.screenshot({ path: path.join(evidence, `config-init-dark-${width}.png`) });
    }
    return results;
  } finally {
    await page.close();
  }
}
