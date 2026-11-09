// Проверка HTML-панели (ui/toolbar.html) в обычном Chrome без CEF:
// window.cefQuery подменяется заглушкой, которая записывает команды.
//
//   npm i -D playwright && npx playwright install chromium
//   node scripts/ui-test.js            # скриншоты появятся в target/ui-test/
//   CHROME_PATH=/usr/bin/google-chrome node scripts/ui-test.js
const { chromium } = require('playwright');
const fs = require('fs');
const path = require('path');

const root = path.resolve(__dirname, '..');
const toolbar = 'file://' + path.join(root, 'ui/toolbar.html');
const newtab = 'file://' + path.join(root, 'ui/newtab.html');
const out = path.join(root, 'target/ui-test');
fs.mkdirSync(out, { recursive: true });
const shot = (name) => path.join(out, name);

const mock = () => {
  window.__requests = [];
  window.__readyState = null;
  window.cefQuery = ({ request, onSuccess }) => {
    const r = JSON.parse(request);
    window.__requests.push(r);
    if (r.cmd === 'ready') setTimeout(() => onSuccess(JSON.stringify(window.__readyState || { tabs: [], active: null })), 0);
    else setTimeout(() => onSuccess(''), 0);
    return 1;
  };
};

const state = {
  active: 2,
  tabs: [
    { id: 1, title: 'Rust Programming Language', url: 'https://www.rust-lang.org/', favicon: null, loading: false, progress: 1, canGoBack: false, canGoForward: false },
    { id: 2, title: 'Chromium Embedded Framework — Wikipedia', url: 'https://en.wikipedia.org/wiki/Chromium_Embedded_Framework', favicon: null, loading: true, progress: 0.45, canGoBack: true, canGoForward: false },
    { id: 3, title: '<img src=x onerror=alert(1)> XSS title', url: 'https://example.com/', favicon: 'javascript:alert(1)', loading: false, progress: 1, canGoBack: true, canGoForward: true },
    { id: 4, title: 'New Tab', url: '', favicon: null, loading: false, progress: 1, canGoBack: false, canGoForward: false },
  ],
};

(async () => {
  const browser = await chromium.launch({ executablePath: process.env.CHROME_PATH || undefined });
  const results = [];
  const check = (name, ok) => { results.push(`${ok ? 'PASS' : 'FAIL'} ${name}`); };

  for (const scheme of ['dark', 'light']) {
    const ctx = await browser.newContext({ viewport: { width: 1100, height: 80 }, colorScheme: scheme, deviceScaleFactor: 2 });
    const page = await ctx.newPage();
    const errors = [];
    page.on('pageerror', (e) => errors.push(e.message));
    page.on('dialog', (d) => { errors.push('dialog: ' + d.message()); d.dismiss(); });
    await page.addInitScript(mock);
    await page.addInitScript((s) => { window.__readyState = s; }, state);
    await page.goto(toolbar);
    await page.waitForTimeout(300);

    if (scheme === 'dark') {
      const reqs = await page.evaluate(() => window.__requests);
      check('panel sends ready on load', reqs[0] && reqs[0].cmd === 'ready');
      check('renders 4 tabs', (await page.locator('.tab').count()) === 4);
      check('active tab highlighted', (await page.locator('.tab.active .title').textContent()) === state.tabs[1].title);
      check('address shows active url', (await page.inputValue('#address')) === state.tabs[1].url);
      check('back enabled / forward disabled', !(await page.isDisabled('#back')) && (await page.isDisabled('#forward')));
      check('reload shows stop while loading', await page.locator('#reload.stop').count() === 1);
      check('XSS title rendered as text', (await page.locator('.tab[data-id="3"] .title').textContent()).startsWith('<img'));
      check('no injected img in title', (await page.locator('.tab[data-id="3"] .title img').count()) === 0);
    }
    await page.screenshot({ path: shot(`toolbar-${scheme}.png`) });

    if (scheme === 'dark') {
      // Клики → команды для Rust.
      await page.evaluate(() => { window.__requests = []; });
      await page.click('.tab[data-id="1"]');
      await page.click('.tab[data-id="3"] .close', { force: true });
      await page.click('#new-tab');
      await page.click('#back');
      await page.click('#reload');
      await page.click('#menu');
      await page.locator('.tab[data-id="4"]').click({ button: 'middle' });
      await page.waitForTimeout(100);
      let reqs = await page.evaluate(() => window.__requests);
      const cmds = reqs.map((r) => r.cmd + (r.id ? ':' + r.id : '')).join(',');
      check(`click commands (${cmds})`, cmds === 'selectTab:1,closeTab:3,newTab,back,stop,showPopup,closeTab:4');

      // Адресная строка: ввод + Enter, Esc.
      await page.evaluate(() => { window.__requests = []; });
      await page.click('#address');
      await page.keyboard.type('rust cef');
      // Пока пользователь печатает, обновление состояния не затирает ввод.
      await page.evaluate((s) => window.rustium.update({ ...s, tabs: s.tabs.map((t) => ({ ...t, progress: 0.9 })) }), state);
      check('typing is not overwritten by updates', (await page.inputValue('#address')) === 'rust cef');
      await page.keyboard.press('Enter');
      await page.waitForTimeout(50);
      reqs = await page.evaluate(() => window.__requests);
      check('enter sends navigate', reqs.some((r) => r.cmd === 'navigate' && r.url === 'rust cef'));
      await page.click('#address');
      await page.keyboard.type('zzz');
      await page.keyboard.press('Escape');
      await page.waitForTimeout(50);
      reqs = await page.evaluate(() => window.__requests);
      check('escape sends focusContent', reqs.some((r) => r.cmd === 'focusContent'));

      // Rust переключил вкладку.
      await page.evaluate((s) => window.rustium.update({ ...s, active: 1 }), state);
      check('switching tab updates address', (await page.inputValue('#address')) === 'https://www.rust-lang.org/');
      check('forward disabled on tab 1', await page.isDisabled('#forward'));

      // focusAddress
      await page.evaluate(() => window.rustium.focusAddress());
      check('focusAddress focuses input', await page.evaluate(() => document.activeElement.id === 'address'));

      // Много вкладок: узкий режим.
      const many = { active: 5, tabs: Array.from({ length: 24 }, (_, i) => ({ ...state.tabs[0], id: i + 1, title: 'Tab ' + (i + 1) })) };
      await page.evaluate((s) => window.rustium.update(s), many);
      const narrow = await page.waitForFunction(() => document.querySelectorAll('.tab.narrow').length > 0, null, { timeout: 2000 }).then(() => true, () => false);
      check('narrow mode with 24 tabs', narrow);
      await page.screenshot({ path: shot('toolbar-many-tabs.png') });

      // Удаление вкладок.
      await page.evaluate((s) => window.rustium.update(s), { active: 2, tabs: [state.tabs[1]] });
      check('removed tabs disappear', (await page.locator('.tab').count()) === 1);
    }
    check(`no JS errors (${scheme})`, errors.length === 0 || (console.log(errors), false));
    await ctx.close();
  }

  // Страница новой вкладки.
  const ctx = await browser.newContext({ viewport: { width: 1100, height: 560 }, colorScheme: 'dark', deviceScaleFactor: 1 });
  const page = await ctx.newPage();
  await page.goto(newtab);
  check('newtab title', (await page.title()) === 'New Tab');
  await page.screenshot({ path: shot('newtab.png') });
  await browser.close();

  console.log(results.join('\n'));
  process.exit(results.some((r) => r.startsWith('FAIL')) ? 1 : 0);
})();
