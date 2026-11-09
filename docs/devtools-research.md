# Штатные DevTools CEF внутри окна: исследование

Ветка `devtools-native`. CEF 156.0.26 (ветка CEF 8078, Chromium 156.0.8078.88),
macOS. Исходники CEF прочитаны в ветке 8078 (HEAD `a10dae3`).

## Вывод

Встроить **штатные** DevTools CEF в окно рядом со страницей в CEF 156 нельзя
без собственной сборки CEF. Поддерживается только отдельное окно DevTools.
DevTools внутри окна возможны только как свой view с фронтендом DevTools,
подключённым к странице по CDP (так сделано в `main`).

## Что мешает (по исходникам CEF)

1. **CEF сам выключает встраивание.** Патч CEF к Chrome
   `patch/patches/chrome_browser_browser.patch`, `DevToolsWindow::Create`:

   ```cpp
   if (can_dock && browser && browser->cef_delegate()) {
     // Don't dock DevTools for CEF-managed browsers.
     can_dock = false;
   }
   ```

   Действует для всех браузеров под управлением CEF, с любым
   `GetChromeToolbarType`. Появился в коммите `53ef570f5` (2023-11, #3282/#3600).

2. **DevTools бывают только в Chrome-стиле.** `libcef/browser/views/browser_view_impl.cc`,
   `ComputeAlloyStyle()`: для DevTools-попапа запрос Alloy логируется
   («only Chrome style is supported for DevTools popups») и игнорируется.
   Свои Alloy-DevTools удалены вместе с Alloy bootstrap (`a461a8972`, 2024-06).

3. **Свой родитель не задать.** `ShowDevTools` игнорирует `windowInfo`, если
   вкладка — BrowserView; `OnBeforeDevToolsPopup` тоже. На macOS браузер с
   `parent_view` всегда Alloy (`cef_types_mac.h`), а DevTools Alloy не бывают.

4. **Chrome-стиль окна не спасает.** Окно Chrome-стиля держит ровно один
   Chrome-браузер и связано с ним (`ChromeBrowserWidget::Init`). Если этим
   браузером сделать DevTools через `OnPopupBrowserViewCreated`, то при их
   закрытии Chrome закрывает окно (пустой tab strip → закрытие Browser).
   Запрет `CanClose` оставляет окно без Browser, и CEF падает при смене фокуса
   (`-[NSWindow resignKeyWindow]`).

## Опыты в этой ветке

| Опыт | Результат |
|---|---|
| Alloy-окно, делегат DevTools с Alloy-стилем | лог «only Chrome style…», падение в `show_dev_tools` |
| `OnPopupBrowserViewCreated` → 1, вставка view через `post_ui` | падение: CEF ждёт view в окне сразу |
| Chrome-окно, DevTools Chrome-стиля встроены рядом со страницей | открываются; при закрытии CEF закрывает окно, с запретом — падение при смене фокуса |
| `RUSTIUM_EXPERIMENT=chrome-tab`: окно и первая вкладка в Chrome-стиле, штатный `ShowDevTools` | **отдельное окно 800×600**, страница осталась 1280 px — Chrome не встраивает (патч `can_dock = false`) |

Опыт `chrome-tab` выявил ещё одно правило: в окне Chrome-стиля вкладка в
Chrome-стиле должна попасть в окно раньше любого Alloy BrowserView, включая
overlay (`AddedToWidget`: «Cannot add multiple Chrome style BrowserViews»).
Запуск опыта:

```sh
RUSTIUM_EXPERIMENT=chrome-tab ./target/bundle/rustium.app/Contents/MacOS/rustium
```

## Какие есть пути

1. **Отдельное окно** — официальный путь (cefclient `views_window.cc`).
   Работает уже сейчас (положение «Separate window» в меню).
2. **Свой view с фронтендом DevTools по CDP** (как в `main`). Этот же способ
   предлагал автор CEF в issue #4016, но как «не поддерживаемый напрямую».
   Нет интеграции с DevToolsHost: меню положения, кнопку закрытия и сохранение
   файлов надо делать самим. Наш мост безопаснее `--remote-debugging-port`:
   только 127.0.0.1 и секретный токен.
3. **Своя сборка CEF без патча `can_dock = false`.** Это сборка Chromium
   (часы и ~100 ГБ), без поддержки, со Views не проверялось.
