# M12a: message catalogue and localisation sweep

Date: 2026-10-17. ADR: `docs/adr/0010-localisation-and-message-catalog.md`. Messages: **664** English, **664** Russian (100 % in CI: `every_message_is_translated`).

## What changed

* New pure crate `crates/gorca-i18n` (catalog files `catalog/{en,ru}/*.msg`, generated `MsgId`, plural rules, extension catalogs and message references with hard caps). Format choice and rationale: ADR-0010 (Fluent is not in the offline registry).
* Every spoken string of the core moved to the catalog: role words (`Role::spoken`), ARIA kind/state/current words, focus and change presentations, browse mode (landmarks, list/table sizes, quick-nav "nothing found" per category with gender and case), command confirmations and hints, input help (`CmdSpec.help` is a `MsgId`), key names and echo names, modifier names, settings messages, input notices ("Insert stuck, tap Insert"), review and object navigation, text layer messages, formatting information (colours), live-region summaries, time and date, lock keys, speech history, sleep mode, battery, spelling alphabets, speech failover notices, extension-disabled notices.
* The `@word.*` and `@level.*` rows left `symbols-<lang>.dic`; `gorca_speech::Lang` is now `gorca_i18n::Lang` (same `from_code` resolution).
* Extensions: `ext-gnome-shell` and `ext-vte` ship `i18n/{en,ru}.msg` (namespaced) and return message references; the host resolves them where extension text enters the presentation; `xtask build-ext` copies the catalogs.
* `cargo xtask strings` (also part of `cargo xtask deps`) is the PR-7 guard; allow-list `xtask/strings.allow`.
* The Reader publishes `speech.language` to the catalog when settings load (`on_loaded`).

## Intentional wording changes (English unchanged elsewhere)

| Where | Before | After | Why |
|---|---|---|---|
| Russian battery percent | "82 процентов" | "82 процента" | real plural forms |
| Russian settings problems | "N settings problem(s)" in English | "в настройках N проблем(а/ы)" | catalogue |
| Russian phonetic spelling | Cyrillic letters named plainly | Russian spelling alphabet | new for Russian |
| `ext-gnome-shell` Russian | none | "N процентов" with plurals | catalogue |

English strings: every English literal of `main` in the converted files is a catalog value or a composition of catalog values (checked by script against the pre-sweep literal list); the existing English unit tests (about 700 in `gorca-core` alone) pass unchanged except one Russian expectation (battery plural).

## Not converted, on purpose

* Settings/keymap diagnostics read by `settings.report_problems` (English; quote paths and values): M14.
* `debug.node_info` developer dump; CLI usage and RT/topology text on stderr; log lines; match patterns (secret prompt words, abbreviations); the extension manifest parser diagnostics. All in `xtask/strings.allow` with a reason.
* Per-segment voice switching by `lang` (M12 proper).

## Russian strings for native review

Every Russian string below was written fresh by the engineer (NVDA's `nvda.po` for `ru` was the reference for role and state words) and **needs a native review**. Rows with a note are the ones I am least sure of.


### commands

| id | English | Russian | review note |
|---|---|---|---|
| `cmd-speech-stop` | stop speech | остановить речь |  |
| `cmd-speech-pause-toggle` | pause or resume speech | приостановить или продолжить речь |  |
| `cmd-speech-mode-cycle` | turn speech on or off | включить или выключить речь |  |
| `cmd-speech-repeat-last` | repeat the last spoken text | повторить последнюю произнесённую фразу |  |
| `cmd-speech-ring-next` | select the next voice setting | выбрать следующий параметр голоса |  |
| `cmd-speech-ring-prev` | select the previous voice setting | выбрать предыдущий параметр голоса |  |
| `cmd-speech-ring-inc` | increase the selected voice setting | увеличить выбранный параметр голоса |  |
| `cmd-speech-ring-dec` | decrease the selected voice setting | уменьшить выбранный параметр голоса |  |
| `cmd-speech-ring-inc-large` | increase the selected voice setting by a large step | увеличить выбранный параметр голоса большим шагом |  |
| `cmd-speech-ring-dec-large` | decrease the selected voice setting by a large step | уменьшить выбранный параметр голоса большим шагом |  |
| `cmd-speech-rate-up` | increase speech rate | увеличить скорость речи |  |
| `cmd-speech-rate-down` | decrease speech rate | уменьшить скорость речи |  |
| `cmd-speech-pitch-up` | increase speech pitch | повысить тон речи |  |
| `cmd-speech-pitch-down` | decrease speech pitch | понизить тон речи |  |
| `cmd-speech-volume-up` | increase speech volume | увеличить громкость речи |  |
| `cmd-speech-volume-down` | decrease speech volume | уменьшить громкость речи |  |
| `cmd-speech-copy-last` | copy the last spoken text | скопировать последнюю произнесённую фразу |  |
| `cmd-speech-punctuation-cycle` | cycle the punctuation level | переключить уровень пунктуации |  |
| `cmd-info-where-am-i` | report the focused object and its surroundings; twice for details, three times to spell the name | сообщить о текущем объекте и его окружении; дважды: подробности, трижды: произнести название по буквам |  |
| `cmd-info-window-title` | report the window title; twice to spell it | сообщить заголовок окна; дважды: произнести по буквам |  |
| `cmd-info-time-date` | report the time; twice for the date | сообщить время; дважды: дату |  |
| `cmd-info-current-line` | report the current line | сообщить текущую строку |  |
| `cmd-info-selection` | report the selected text | сообщить выделенный текст |  |
| `cmd-info-formatting` | report text formatting | сообщить форматирование текста |  |
| `cmd-info-status-bar` | report the status bar | сообщить строку состояния |  |
| `cmd-info-read-window` | read the whole window | прочитать всё окно |  |
| `cmd-info-battery` | report the battery state | сообщить состояние батареи |  |
| `cmd-info-focus-shortcut` | report the shortcut of the focused object | сообщить сочетание клавиш текущего объекта |  |
| `cmd-info-clipboard` | report the clipboard text | сообщить текст в буфере обмена |  |
| `cmd-info-link-destination` | report the link destination | сообщить адрес ссылки |  |
| `cmd-info-details` | report details of the object | сообщить подробности об объекте |  |
| `cmd-echo-typed-chars-toggle` | toggle speaking typed characters | включить или выключить озвучивание вводимых символов |  |
| `cmd-echo-typed-words-toggle` | toggle speaking typed words | включить или выключить озвучивание вводимых слов |  |
| `cmd-echo-command-keys-toggle` | toggle speaking command keys | включить или выключить озвучивание командных клавиш |  |
| `cmd-verbosity-cycle` | cycle the verbosity level | переключить уровень подробности |  |
| `cmd-input-learn-mode-toggle` | turn learn mode on or off | включить или выключить режим обучения |  |
| `cmd-input-pass-next-key` | send the next key to the application | передать следующую клавишу приложению |  |
| `cmd-input-sleep-toggle` | turn sleep mode on or off for this application | включить или выключить режим сна для этого приложения |  |
| `cmd-settings-reload` | reload the settings file and discard unsaved changes | перечитать файл настроек и отбросить несохранённые изменения |  |
| `cmd-settings-save` | save the current settings | сохранить текущие настройки |  |
| `cmd-settings-report-problems` | report problems found in the settings file | сообщить о проблемах в файле настроек |  |
| `cmd-debug-node-info` | report debug information about the focused object | сообщить отладочные сведения о текущем объекте |  |
| `cmd-debug-app-info` | report debug information about the application | сообщить отладочные сведения о приложении |  |
| `cmd-gorca-quit` | quit the screen reader | выйти из программы экранного доступа |  |
| `cmd-text-say-all` | read from here to the end | читать отсюда до конца |  |
| `cmd-text-say-char` | report the current character | сообщить текущий символ |  |
| `cmd-text-say-word` | report the current word | сообщить текущее слово |  |
| `cmd-text-say-line` | report the current line | сообщить текущую строку |  |
| `cmd-text-spell-char` | spell the current character | произнести текущий символ по буквам |  |
| `cmd-text-spell-word` | spell the current word | произнести текущее слово по буквам |  |
| `cmd-text-spell-line` | spell the current line | произнести текущую строку по буквам |  |
| `cmd-review-prev-line` | move the review cursor to the previous line | переместить курсор просмотра на предыдущую строку |  |
| `cmd-review-cur-line` | report the review line | сообщить строку просмотра |  |
| `cmd-review-next-line` | move the review cursor to the next line | переместить курсор просмотра на следующую строку |  |
| `cmd-review-prev-word` | move the review cursor to the previous word | переместить курсор просмотра на предыдущее слово |  |
| `cmd-review-cur-word` | report the review word | сообщить слово просмотра |  |
| `cmd-review-next-word` | move the review cursor to the next word | переместить курсор просмотра на следующее слово |  |
| `cmd-review-prev-char` | move the review cursor to the previous character | переместить курсор просмотра на предыдущий символ |  |
| `cmd-review-cur-char` | report the review character | сообщить символ просмотра |  |
| `cmd-review-next-char` | move the review cursor to the next character | переместить курсор просмотра на следующий символ |  |
| `cmd-review-top` | move the review cursor to the top | переместить курсор просмотра в начало |  |
| `cmd-review-bottom` | move the review cursor to the bottom | переместить курсор просмотра в конец |  |
| `cmd-review-start-of-line` | move the review cursor to the start of the line | переместить курсор просмотра в начало строки |  |
| `cmd-review-end-of-line` | move the review cursor to the end of the line | переместить курсор просмотра в конец строки |  |
| `cmd-review-prev-page` | move the review cursor up a page | переместить курсор просмотра на страницу вверх |  |
| `cmd-review-next-page` | move the review cursor down a page | переместить курсор просмотра на страницу вниз |  |
| `cmd-review-say-all` | read from the review cursor | читать от курсора просмотра |  |
| `cmd-review-mode-next` | switch to the next review mode | перейти к следующему режиму просмотра |  |
| `cmd-review-mode-prev` | switch to the previous review mode | перейти к предыдущему режиму просмотра |  |
| `cmd-review-to-focus` | move the review cursor to the focus | переместить курсор просмотра к фокусу |  |
| `cmd-review-move-focus` | move the focus to the review cursor | переместить фокус к курсору просмотра |  |
| `cmd-review-activate` | activate the object under the review cursor | активировать объект под курсором просмотра |  |
| `cmd-review-start-of-selection` | move the review cursor to the start of the selection | переместить курсор просмотра в начало выделения |  |
| `cmd-review-end-of-selection` | move the review cursor to the end of the selection | переместить курсор просмотра в конец выделения |  |
| `cmd-review-copy-mark` | mark the review start for copying | отметить начало просмотра для копирования |  |
| `cmd-review-copy` | copy from the mark to the review cursor | скопировать от отметки до курсора просмотра |  |
| `cmd-review-toggle` | turn review mode on or off | включить или выключить режим просмотра |  |
| `cmd-review-caret-follows-toggle` | toggle the caret following the review cursor | включить или выключить следование каретки за курсором просмотра |  |
| `cmd-review-focus-follows-toggle` | toggle the review cursor following the focus | включить или выключить следование курсора просмотра за фокусом |  |
| `cmd-review-simple-toggle` | toggle simple review, which skips layout-only objects | включить или выключить простой просмотр, пропускающий служебные объекты |  |
| `cmd-review-above` | move the review cursor to the zone above | переместить курсор просмотра в зону выше |  |
| `cmd-review-below` | move the review cursor to the zone below | переместить курсор просмотра в зону ниже |  |
| `cmd-object-current` | report the current object | сообщить текущий объект |  |
| `cmd-object-parent` | move to the parent object | перейти к родительскому объекту |  |
| `cmd-object-first-child` | move to the first child object | перейти к первому дочернему объекту |  |
| `cmd-object-next` | move to the next object | перейти к следующему объекту |  |
| `cmd-object-prev` | move to the previous object | перейти к предыдущему объекту |  |
| `cmd-object-next-in-flow` | move to the next object in reading order | перейти к следующему объекту в порядке чтения |  |
| `cmd-object-prev-in-flow` | move to the previous object in reading order | перейти к предыдущему объекту в порядке чтения |  |
| `cmd-browse-toggle` | toggle between browse mode and focus mode | переключиться между режимом просмотра и режимом фокуса |  |
| `cmd-browse-single-letter-toggle` | toggle single letter navigation | включить или выключить навигацию одной буквой |  |
| `cmd-browse-elements-list` | list elements of the document | показать список элементов документа |  |
| `cmd-browse-find` | find text in the document | найти текст в документе |  |
| `cmd-browse-find-next` | find the next match | найти следующее совпадение |  |
| `cmd-browse-find-prev` | find the previous match | найти предыдущее совпадение |  |
| `cmd-browse-refresh` | refresh the document | обновить документ |  |
| `cmd-browse-live-regions-toggle` | toggle live region announcements | включить или выключить объявления динамических областей |  |
| `cmd-browse-activate` | activate the object | активировать объект |  |
| `cmd-browse-container-start` | move to the start of the container | перейти в начало контейнера |  |
| `cmd-browse-container-end` | move to the end of the container | перейти в конец контейнера |  |
| `cmd-browse-escape` | return from focus mode to browse mode | вернуться из режима фокуса в режим просмотра |  |
| `cmd-browse-cursor-next-line` | move the virtual cursor to the next line | переместить виртуальный курсор на следующую строку |  |
| `cmd-browse-cursor-prev-line` | move the virtual cursor to the previous line | переместить виртуальный курсор на предыдущую строку |  |
| `cmd-browse-cursor-next-char` | move the virtual cursor to the next character | переместить виртуальный курсор на следующий символ |  |
| `cmd-browse-cursor-prev-char` | move the virtual cursor to the previous character | переместить виртуальный курсор на предыдущий символ |  |
| `cmd-browse-cursor-next-word` | move the virtual cursor to the next word | переместить виртуальный курсор на следующее слово |  |
| `cmd-browse-cursor-prev-word` | move the virtual cursor to the previous word | переместить виртуальный курсор на предыдущее слово |  |
| `cmd-browse-cursor-line-start` | move the virtual cursor to the start of the line | переместить виртуальный курсор в начало строки |  |
| `cmd-browse-cursor-line-end` | move the virtual cursor to the end of the line | переместить виртуальный курсор в конец строки |  |
| `cmd-browse-cursor-top` | move the virtual cursor to the top of the document | переместить виртуальный курсор в начало документа |  |
| `cmd-browse-cursor-bottom` | move the virtual cursor to the bottom of the document | переместить виртуальный курсор в конец документа |  |
| `cmd-browse-cursor-page-up` | move the virtual cursor up one page | переместить виртуальный курсор на страницу вверх |  |
| `cmd-browse-cursor-page-down` | move the virtual cursor down one page | переместить виртуальный курсор на страницу вниз |  |
| `cmd-quicknav-next-heading` | move to the next heading | перейти к следующему заголовку |  |
| `cmd-quicknav-prev-heading` | move to the previous heading | перейти к предыдущему заголовку |  |
| `cmd-quicknav-next-heading1` | move to the next heading1 | перейти к следующему заголовку уровня 1 |  |
| `cmd-quicknav-prev-heading1` | move to the previous heading1 | перейти к предыдущему заголовку уровня 1 |  |
| `cmd-quicknav-next-heading2` | move to the next heading2 | перейти к следующему заголовку уровня 2 |  |
| `cmd-quicknav-prev-heading2` | move to the previous heading2 | перейти к предыдущему заголовку уровня 2 |  |
| `cmd-quicknav-next-heading3` | move to the next heading3 | перейти к следующему заголовку уровня 3 |  |
| `cmd-quicknav-prev-heading3` | move to the previous heading3 | перейти к предыдущему заголовку уровня 3 |  |
| `cmd-quicknav-next-heading4` | move to the next heading4 | перейти к следующему заголовку уровня 4 |  |
| `cmd-quicknav-prev-heading4` | move to the previous heading4 | перейти к предыдущему заголовку уровня 4 |  |
| `cmd-quicknav-next-heading5` | move to the next heading5 | перейти к следующему заголовку уровня 5 |  |
| `cmd-quicknav-prev-heading5` | move to the previous heading5 | перейти к предыдущему заголовку уровня 5 |  |
| `cmd-quicknav-next-heading6` | move to the next heading6 | перейти к следующему заголовку уровня 6 |  |
| `cmd-quicknav-prev-heading6` | move to the previous heading6 | перейти к предыдущему заголовку уровня 6 |  |
| `cmd-quicknav-next-heading7` | move to the next heading7 | перейти к следующему заголовку уровня 7 |  |
| `cmd-quicknav-prev-heading7` | move to the previous heading7 | перейти к предыдущему заголовку уровня 7 |  |
| `cmd-quicknav-next-heading8` | move to the next heading8 | перейти к следующему заголовку уровня 8 |  |
| `cmd-quicknav-prev-heading8` | move to the previous heading8 | перейти к предыдущему заголовку уровня 8 |  |
| `cmd-quicknav-next-heading9` | move to the next heading9 | перейти к следующему заголовку уровня 9 |  |
| `cmd-quicknav-prev-heading9` | move to the previous heading9 | перейти к предыдущему заголовку уровня 9 |  |
| `cmd-quicknav-next-link` | move to the next link | перейти к следующей ссылке |  |
| `cmd-quicknav-prev-link` | move to the previous link | перейти к предыдущей ссылке |  |
| `cmd-quicknav-next-visited` | move to the next visited | перейти к следующей посещённой ссылке |  |
| `cmd-quicknav-prev-visited` | move to the previous visited | перейти к предыдущей посещённой ссылке |  |
| `cmd-quicknav-next-unvisited` | move to the next unvisited | перейти к следующей непосещённой ссылке |  |
| `cmd-quicknav-prev-unvisited` | move to the previous unvisited | перейти к предыдущей непосещённой ссылке |  |
| `cmd-quicknav-next-form-field` | move to the next form field | перейти к следующему полю формы |  |
| `cmd-quicknav-prev-form-field` | move to the previous form field | перейти к предыдущему полю формы |  |
| `cmd-quicknav-next-list` | move to the next list | перейти к следующему списку |  |
| `cmd-quicknav-prev-list` | move to the previous list | перейти к предыдущему списку |  |
| `cmd-quicknav-next-list-item` | move to the next list item | перейти к следующему элементу списка |  |
| `cmd-quicknav-prev-list-item` | move to the previous list item | перейти к предыдущему элементу списка |  |
| `cmd-quicknav-next-button` | move to the next button | перейти к следующей кнопке |  |
| `cmd-quicknav-prev-button` | move to the previous button | перейти к предыдущей кнопке |  |
| `cmd-quicknav-next-edit` | move to the next edit | перейти к следующему полю ввода |  |
| `cmd-quicknav-prev-edit` | move to the previous edit | перейти к предыдущему полю ввода |  |
| `cmd-quicknav-next-frame` | move to the next frame | перейти к следующему фрейму |  |
| `cmd-quicknav-prev-frame` | move to the previous frame | перейти к предыдущему фрейму |  |
| `cmd-quicknav-next-separator` | move to the next separator | перейти к следующему разделителю |  |
| `cmd-quicknav-prev-separator` | move to the previous separator | перейти к предыдущему разделителю |  |
| `cmd-quicknav-next-radio` | move to the next radio | перейти к следующей радиокнопке |  |
| `cmd-quicknav-prev-radio` | move to the previous radio | перейти к предыдущей радиокнопке |  |
| `cmd-quicknav-next-combobox` | move to the next combobox | перейти к следующему комбинированному списку |  |
| `cmd-quicknav-prev-combobox` | move to the previous combobox | перейти к предыдущему комбинированному списку |  |
| `cmd-quicknav-next-checkbox` | move to the next checkbox | перейти к следующему флажку |  |
| `cmd-quicknav-prev-checkbox` | move to the previous checkbox | перейти к предыдущему флажку |  |
| `cmd-quicknav-next-graphic` | move to the next graphic | перейти к следующему изображению |  |
| `cmd-quicknav-prev-graphic` | move to the previous graphic | перейти к предыдущему изображению |  |
| `cmd-quicknav-next-blockquote` | move to the next blockquote | перейти к следующей цитате |  |
| `cmd-quicknav-prev-blockquote` | move to the previous blockquote | перейти к предыдущей цитате |  |
| `cmd-quicknav-next-not-link-block` | move to the next not link block | перейти к следующему блоку текста без ссылок |  |
| `cmd-quicknav-prev-not-link-block` | move to the previous not link block | перейти к предыдущему блоку текста без ссылок |  |
| `cmd-quicknav-next-landmark` | move to the next landmark | перейти к следующему ориентиру |  |
| `cmd-quicknav-prev-landmark` | move to the previous landmark | перейти к предыдущему ориентиру |  |
| `cmd-quicknav-next-embedded` | move to the next embedded | перейти к следующему встроенному объекту |  |
| `cmd-quicknav-prev-embedded` | move to the previous embedded | перейти к предыдущему встроенному объекту |  |
| `cmd-quicknav-next-annotation` | move to the next annotation | перейти к следующей аннотации |  |
| `cmd-quicknav-prev-annotation` | move to the previous annotation | перейти к предыдущей аннотации |  |
| `cmd-quicknav-next-error` | move to the next error | перейти к следующей ошибке |  |
| `cmd-quicknav-prev-error` | move to the previous error | перейти к предыдущей ошибке |  |
| `cmd-quicknav-next-paragraph` | move to the next paragraph | перейти к следующему абзацу |  |
| `cmd-quicknav-prev-paragraph` | move to the previous paragraph | перейти к предыдущему абзацу |  |
| `cmd-quicknav-next-table` | move to the next table | перейти к следующей таблице |  |
| `cmd-quicknav-prev-table` | move to the previous table | перейти к предыдущей таблице |  |
| `cmd-quicknav-next-clickable` | move to the next clickable | перейти к следующему элементу с действием по щелчку |  |
| `cmd-quicknav-prev-clickable` | move to the previous clickable | перейти к предыдущему элементу с действием по щелчку |  |
| `cmd-quicknav-next-slider` | move to the next slider | перейти к следующему ползунку |  |
| `cmd-quicknav-prev-slider` | move to the previous slider | перейти к предыдущему ползунку |  |
| `cmd-quicknav-next-article` | move to the next article | перейти к следующей статье |  |
| `cmd-quicknav-prev-article` | move to the previous article | перейти к предыдущей статье |  |
| `cmd-quicknav-next-grouping` | move to the next grouping | перейти к следующей группе |  |
| `cmd-quicknav-prev-grouping` | move to the previous grouping | перейти к предыдущей группе |  |
| `cmd-quicknav-next-tab` | move to the next tab | перейти к следующей вкладке |  |
| `cmd-quicknav-prev-tab` | move to the previous tab | перейти к предыдущей вкладке |  |
| `cmd-quicknav-next-figure` | move to the next figure | перейти к следующей иллюстрации |  |
| `cmd-quicknav-prev-figure` | move to the previous figure | перейти к предыдущей иллюстрации |  |
| `cmd-quicknav-next-menu-item` | move to the next menu item | перейти к следующему пункту меню |  |
| `cmd-quicknav-prev-menu-item` | move to the previous menu item | перейти к предыдущему пункту меню |  |
| `cmd-quicknav-next-toggle-button` | move to the next toggle button | перейти к следующей кнопке-переключателю |  |
| `cmd-quicknav-prev-toggle-button` | move to the previous toggle button | перейти к предыдущей кнопке-переключателю |  |
| `cmd-quicknav-next-progress-bar` | move to the next progress bar | перейти к следующему индикатору выполнения |  |
| `cmd-quicknav-prev-progress-bar` | move to the previous progress bar | перейти к предыдущему индикатору выполнения |  |
| `cmd-quicknav-next-math` | move to the next math | перейти к следующей формуле |  |
| `cmd-quicknav-prev-math` | move to the previous math | перейти к предыдущей формуле |  |
| `cmd-quicknav-next-live-region` | move to the next live region | перейти к следующей динамической области |  |
| `cmd-quicknav-prev-live-region` | move to the previous live region | перейти к предыдущей динамической области |  |
| `cmd-quicknav-next-last-live-region` | move to the next last live region | перейти к следующей последней динамической области |  |
| `cmd-quicknav-prev-last-live-region` | move to the previous last live region | перейти к предыдущей последней динамической области |  |
| `cmd-quicknav-next-same-style` | move to the next same style | перейти к следующему тексту того же стиля |  |
| `cmd-quicknav-prev-same-style` | move to the previous same style | перейти к предыдущему тексту того же стиля |  |
| `cmd-quicknav-next-different-style` | move to the next different style | перейти к следующему тексту другого стиля |  |
| `cmd-quicknav-prev-different-style` | move to the previous different style | перейти к предыдущему тексту другого стиля |  |
| `cmd-quicknav-next-reference` | move to the next reference | перейти к следующей сноске |  |
| `cmd-quicknav-prev-reference` | move to the previous reference | перейти к предыдущей сноске |  |
| `cmd-table-next-row` | table: next row | таблица: следующая строка |  |
| `cmd-table-prev-row` | table: previous row | таблица: предыдущая строка |  |
| `cmd-table-next-column` | table: next column | таблица: следующий столбец |  |
| `cmd-table-prev-column` | table: previous column | таблица: предыдущий столбец |  |
| `cmd-table-first-row` | table: first row | таблица: первая строка |  |
| `cmd-table-last-row` | table: last row | таблица: последняя строка |  |
| `cmd-table-first-column` | table: first column | таблица: первый столбец |  |
| `cmd-table-last-column` | table: last column | таблица: последний столбец |  |
| `cmd-table-say-row` | table: read the row | таблица: прочитать строку |  |
| `cmd-table-say-column` | table: read the column | таблица: прочитать столбец |  |
| `cmd-table-say-all-row` | table: read all of the row | таблица: прочитать всю строку |  |
| `cmd-table-say-all-column` | table: read all of the column | таблица: прочитать весь столбец |  |
| `cmd-browse-select-next-char` | extend the selection by a character | расширить выделение на символ |  |
| `cmd-browse-select-prev-char` | shrink or extend the selection back by a character | сократить или расширить выделение назад на символ |  |
| `cmd-browse-select-next-word` | extend the selection by a word | расширить выделение на слово |  |
| `cmd-browse-select-prev-word` | extend the selection back by a word | расширить выделение назад на слово |  |
| `cmd-browse-select-next-line` | extend the selection down a line | расширить выделение вниз на строку |  |
| `cmd-browse-select-prev-line` | extend the selection up a line | расширить выделение вверх на строку |  |
| `cmd-browse-select-line-start` | select to the start of the line | выделить до начала строки |  |
| `cmd-browse-select-line-end` | select to the end of the line | выделить до конца строки |  |
| `cmd-browse-select-top` | select to the start of the document | выделить до начала документа |  |
| `cmd-browse-select-bottom` | select to the end of the document | выделить до конца документа |  |
| `cmd-browse-select-page-up` | select up one page | выделить на страницу вверх |  |
| `cmd-browse-select-page-down` | select down one page | выделить на страницу вниз |  |
| `cmd-browse-select-all` | select the whole document | выделить весь документ |  |
| `cmd-browse-cursor-next-paragraph` | move to the next paragraph | перейти к следующему абзацу |  |
| `cmd-browse-cursor-prev-paragraph` | move to the previous paragraph | перейти к предыдущему абзацу |  |
| `cmd-browse-cursor-next-sentence` | move to the next sentence | перейти к следующему предложению |  |
| `cmd-browse-cursor-prev-sentence` | move to the previous sentence | перейти к предыдущему предложению |  |
| `cmd-browse-tab-next` | focus the next focusable element after the browse cursor | перейти к следующему элементу с фокусом после курсора просмотра |  |
| `cmd-browse-tab-prev` | focus the previous focusable element before the browse cursor | перейти к предыдущему элементу с фокусом перед курсором просмотра |  |
| `cmd-browse-native-selection-toggle` | toggle native selection mode | включить или выключить системное выделение |  |
| `cmd-speech-history-prev` | read the previous spoken text from the speech history | прочитать предыдущую фразу из истории речи |  |
| `cmd-speech-history-next` | read the next spoken text from the speech history | прочитать следующую фразу из истории речи |  |

### notices

| id | English | Russian | review note |
|---|---|---|---|
| `speech-fallback` | {from} unavailable, using {to} | {from} недоступен, используется {to} |  |
| `speech-restored` | {backend} restored | {backend} восстановлен |  |
| `speech-unavailable` | speech service unavailable | служба речи недоступна |  |
| `ext-disabled` | Extension {name} disabled | Расширение {name} отключено |  |
| `ext-disabled-for` | Extension {name} disabled for {app} | Расширение {name} отключено для {app} |  |

### review

| id | English | Russian | review note |
|---|---|---|---|
| `review-top` | Top | Верх |  |
| `review-bottom` | Bottom | Низ |  |
| `review-bottom-truncated` | Bottom, more content not shown | Низ, часть содержимого не показана |  |
| `review-off` | review off | просмотр выключен |  |
| `review-on` | review on | просмотр включён |  |
| `review-simple-on` | simple review on | простой просмотр включён |  |
| `review-simple-off` | simple review off | простой просмотр выключен |  |
| `review-follows-focus-on` | review follows focus on | просмотр следует за фокусом |  |
| `review-follows-focus-off` | review follows focus off | просмотр не следует за фокусом |  |
| `review-position-lost` | review position lost | положение просмотра потеряно |  |
| `review-not-responding` | not responding | не отвечает |  |
| `review-object-gone` | object is gone | объект исчез |  |
| `review-unavailable` | review unavailable | просмотр недоступен |  |
| `review-layout-unavailable` | layout unavailable | раскладка недоступна |  |
| `review-not-active` | review is not active | просмотр не активен |  |
| `review-focus` | focus | фокус |  |
| `review-cannot-move-focus` | cannot move focus here | невозможно переместить фокус сюда |  |
| `review-cannot-activate` | cannot activate | невозможно активировать |  |
| `review-status-bar` | status bar | строка состояния |  |
| `review-no-status-bar` | no status bar found | строка состояния не найдена |  |
| `nav-no-parent` | No containing object | Нет объемлющего объекта |  |
| `nav-no-child` | No objects inside | Нет вложенных объектов |  |
| `nav-no-next` | No next | Нет следующего |  |
| `nav-no-prev` | No previous | Нет предыдущего |  |

### roles

| id | English | Russian | review note |
|---|---|---|---|
| `role-unknown` |  |  |  |
| `role-application` | application | приложение |  |
| `role-window` | window | окно |  |
| `role-dialog` | dialog | диалог |  |
| `role-panel` | panel | панель |  |
| `role-document` | document | документ |  |
| `role-heading` | heading | заголовок |  |
| `role-paragraph` | paragraph | абзац |  |
| `role-link` | link | ссылка |  |
| `role-image` | image | изображение |  |
| `role-list` | list | список |  |
| `role-list-item` | list item | элемент списка |  |
| `role-table` | table | таблица |  |
| `role-row` | row | строка |  |
| `role-cell` | cell | ячейка |  |
| `role-column-header` | column header | заголовок столбца |  |
| `role-row-header` | row header | заголовок строки |  |
| `role-button` | button | кнопка |  |
| `role-toggle-button` | toggle button | кнопка-переключатель |  |
| `role-check-box` | check box | флажок |  |
| `role-radio-button` | radio button | радиокнопка |  |
| `role-combo-box` | combo box | комбинированный список |  |
| `role-text-entry` | edit | поле ввода | differs from NVDA ("редактор") |
| `role-password-entry` | password edit | поле ввода пароля | differs from NVDA |
| `role-spin-button` | spin button | счётчик |  |
| `role-slider` | slider | ползунок |  |
| `role-progress-bar` | progress bar | индикатор выполнения |  |
| `role-menu` | menu | меню |  |
| `role-menu-bar` | menu bar | строка меню |  |
| `role-menu-item` | menu item | пункт меню |  |
| `role-popover` | popover | всплывающее окно | no NVDA equivalent |
| `role-tab` | tab | вкладка |  |
| `role-tab-list` | tab list | панель вкладок | no NVDA equivalent |
| `role-tree` | tree | дерево |  |
| `role-tree-item` | tree item | элемент дерева |  |
| `role-toolbar` | toolbar | панель инструментов |  |
| `role-status-bar` | status bar | строка состояния |  |
| `role-separator` | separator | разделитель |  |
| `role-label` | label | метка |  |
| `role-static-text` | text | текст |  |
| `role-terminal` | terminal | терминал |  |
| `role-landmark` | landmark | ориентир |  |
| `role-alert` | alert | уведомление |  |
| `aria-switch` | switch | переключатель |  |
| `aria-tab` | tab | вкладка |  |
| `aria-tab-list` | tab control | панель вкладок |  |
| `aria-tab-panel` | property page | страница свойств |  |
| `aria-tree` | tree view | дерево |  |
| `aria-tree-item` | tree view item | элемент дерева |  |
| `aria-menu` | menu | меню |  |
| `aria-menu-bar` | menu bar | строка меню |  |
| `aria-menu-item` | menu item | пункт меню |  |
| `aria-check-menu-item` | check menu item | пункт меню с флажком |  |
| `aria-radio-menu-item` | radio menu item | пункт меню с радиокнопкой |  |
| `aria-toolbar` | tool bar | панель инструментов |  |
| `aria-group` | grouping | группа |  |
| `state-unavailable` | unavailable | недоступно |  |
| `state-selected` | selected | выделено |  |
| `state-not-selected` | not selected | не выделено |  |
| `state-busy` | busy | занято |  |
| `state-pressed` | pressed | нажата |  |
| `state-not-pressed` | not pressed | не нажата |  |
| `state-half-pressed` | half pressed | частично нажата |  |
| `state-checked` | checked | отмечено |  |
| `state-not-checked` | not checked | не отмечено |  |
| `state-half-checked` | half checked | частично отмечено |  |
| `state-read-only` | read only | только чтение |  |
| `state-expanded` | expanded | развёрнуто |  |
| `state-collapsed` | collapsed | свёрнуто |  |
| `state-not-expanded` | not expanded | не развёрнуто |  |
| `state-required` | required | обязательно |  |
| `state-invalid-entry` | invalid entry | недопустимое значение | NVDA says "недействительные данные" |
| `state-indeterminate` | indeterminate | неопределённо |  |
| `state-on` | on | включено |  |
| `state-off` | off | выключено |  |
| `state-submenu` | submenu | подменю |  |
| `state-opens-dialog` | opens dialog | открывает диалог |  |
| `state-opens-grid` | opens grid | открывает сетку |  |
| `state-opens-list` | opens list | открывает список |  |
| `state-opens-tree` | opens tree | открывает дерево |  |
| `state-has-popup` | has popup | есть всплывающее меню |  |
| `state-not` | not {word} | не {word} | mechanical negation: "не {state}" reads oddly for some states |
| `state-current` | current | текущий |  |
| `state-current-page` | current page | текущая страница |  |
| `state-current-step` | current step | текущий шаг |  |
| `state-current-location` | current location | текущее место | NVDA: "текущая позиция" |
| `state-current-date` | current date | текущая дата |  |
| `state-current-time` | current time | текущее время |  |
| `state-level` | level {l} | уровень {l} |  |

### spell

| id | English | Russian | review note |
|---|---|---|---|
| `spell-latin` | alpha\|bravo\|charlie\|delta\|echo\|foxtrot\|golf\|hotel\|india\|juliet\|kilo\|lima\|mike\|november\|oscar\|papa\|quebec\|romeo\|sierra\|tango\|uniform\|victor\|whiskey\|x-ray\|yankee\|zulu | альфа\|браво\|чарли\|дельта\|эко\|фокстрот\|гольф\|отель\|индия\|джульетта\|кило\|лима\|майк\|ноябрь\|оскар\|папа\|кебек\|ромео\|сьерра\|танго\|юниформ\|виктор\|виски\|икс-рэй\|янки\|зулу | Russian transliteration of the NATO alphabet |
| `spell-cyrillic` |  | Анна\|Борис\|Василий\|Григорий\|Дмитрий\|Елена\|Ёлка\|Женя\|Зинаида\|Иван\|Иван краткий\|Константин\|Леонид\|Михаил\|Николай\|Ольга\|Павел\|Роман\|Семён\|Татьяна\|Ульяна\|Фёдор\|Харитон\|Цецилия\|Человек\|Шура\|Щука\|твёрдый знак\|Ы\|мягкий знак\|Эхо\|Юрий\|Яков | Russian telephone spelling alphabet (new in Russian) |

### system

| id | English | Russian | review note |
|---|---|---|---|
| `speech-off` | speech off | речь выключена |  |
| `speech-on` | speech on | речь включена |  |
| `nothing-to-repeat` | nothing to repeat | нечего повторять |  |
| `cmd-typed-chars-on` | speak typed characters on | озвучивание вводимых символов включено |  |
| `cmd-typed-chars-off` | speak typed characters off | озвучивание вводимых символов выключено |  |
| `cmd-typed-words-on` | speak typed words on | озвучивание вводимых слов включено |  |
| `cmd-typed-words-off` | speak typed words off | озвучивание вводимых слов выключено |  |
| `cmd-command-keys-on` | speak command keys on | озвучивание командных клавиш включено |  |
| `cmd-command-keys-off` | speak command keys off | озвучивание командных клавиш выключено |  |
| `verbosity-confirm` | verbosity {name} | подробность: {name} |  |
| `verbosity-brief` | brief | краткая |  |
| `verbosity-normal` | normal | обычная |  |
| `verbosity-verbose` | verbose | подробная |  |
| `learn-on` | learn mode on, Escape to exit | режим обучения включён, Escape для выхода |  |
| `learn-on-brief` | learn on | обучение включено |  |
| `learn-off` | learn mode off | режим обучения выключен |  |
| `learn-off-brief` | learn off | обучение выключено |  |
| `pass-next` | pass next key | следующая клавиша передаётся приложению |  |
| `pass-next-brief` | pass | передача |  |
| `pass-cancelled` | pass next key cancelled | передача следующей клавиши отменена |  |
| `settings-not-saved-newer` | not saved: settings file is from a newer version | не сохранено: файл настроек создан более новой версией |  |
| `settings-not-saved-changed` | not saved: settings file changed on disk | не сохранено: файл настроек изменился на диске |  |
| `settings-save-failed` | settings could not be saved | не удалось сохранить настройки |  |
| `settings-saved` | settings saved | настройки сохранены |  |
| `settings-no-problems` | no settings problems | в настройках нет проблем |  |
| `settings-problems` | {n} settings problem{n#\|s} | в настройках {n} {n#проблема\|проблемы\|проблем} |  |
| `settings-reloaded` | settings reloaded | настройки перечитаны |  |
| `settings-reloaded-problems` | settings reloaded, {n} problem{n#\|s}. Press gorca control shift R for details | настройки перечитаны, {n} {n#проблема\|проблемы\|проблем}. Нажмите gorca, control, shift, R для подробностей | the chord is spelled out, check wording |
| `settings-startup-problems` | Settings: {n} problem{n#\|s} | Настройки: {n} {n#проблема\|проблемы\|проблем} |  |
| `command-not-available` | command not available yet | команда пока недоступна |  |
| `no-focus` | no focus | нет фокуса |  |
| `focus-gone` | focused object is gone | объект с фокусом исчез |  |
| `window-title-unavailable` | window title not available | заголовок окна недоступен |  |
| `app-not-responding` | application not responding | приложение не отвечает |  |
| `input-help-line` | {key}: {help} | {key}: {help} |  |
| `input-help-planned` | {help}, not available yet | {help}, пока недоступно |  |
| `input-help-unbound` | not bound | не назначено |  |
| `input-held-by-other` | Another screen reader holds the keyboard; gorca commands are unavailable | Клавиатуру удерживает другая программа экранного доступа; команды gorca недоступны |  |
| `input-unavailable` | keyboard commands unavailable | команды клавиатуры недоступны |  |
| `input-restored` | keyboard commands restored | команды клавиатуры восстановлены |  |
| `input-stuck` | {name} stuck, tap {name} | {name} залипла, нажмите {name} |  |
| `input-key-insert` | Insert | Insert | kept in Latin |
| `input-key-kp-insert` | Keypad Insert | Insert на цифровой клавиатуре |  |
| `input-key-caps-lock` | Caps Lock | Caps Lock | kept in Latin |
| `input-key-gorca` | the gorca key | клавиша gorca | product name |
| `input-caps-dropped` | Caps Lock is used for something else, so the gorca key is Insert | Caps Lock занята другой функцией, поэтому клавиша gorca — Insert |  |
| `mod-gorca` | gorca | горка | product name read phonetically |
| `mod-control` | control | контрол | transliterated modifier names; check against the Russian voice |
| `mod-alt` | alt | альт | transliterated modifier names |
| `mod-super` | super | супер | transliterated modifier names |
| `mod-shift` | shift | шифт | transliterated modifier names |
| `key-again` | again | ещё раз |  |
| `key-gorca-passed` | gorca key passed to application | клавиша gorca передана приложению |  |
| `ring-rate` | rate | скорость |  |
| `ring-pitch` | pitch | тон |  |
| `ring-volume` | volume | громкость |  |
| `ring-say` | {name} {v} | {name} {v} |  |
| `word-space` | space | пробел |  |
| `unknown` | unknown | неизвестно |  |
| `info-position` | {at} of {of} | {at} из {of} |  |
| `info-no-name` | {role}, no name | {role}, без названия |  |
| `info-out-of-date` | information may be out of date | сведения могли устареть |  |
| `time-am` | AM | утра | 12-hour clock words |
| `time-pm` | PM | вечера | 12-hour clock words |
| `time-12h` | {h}:{m} {ap} | {h}:{m} {ap} |  |
| `time-24h` | {h}:{m} | {h}:{m} |  |
| `month-1` | January | января |  |
| `month-2` | February | февраля |  |
| `month-3` | March | марта |  |
| `month-4` | April | апреля |  |
| `month-5` | May | мая |  |
| `month-6` | June | июня |  |
| `month-7` | July | июля |  |
| `month-8` | August | августа |  |
| `month-9` | September | сентября |  |
| `month-10` | October | октября |  |
| `month-11` | November | ноября |  |
| `month-12` | December | декабря |  |
| `day-1` | Monday | понедельник |  |
| `day-2` | Tuesday | вторник |  |
| `day-3` | Wednesday | среда |  |
| `day-4` | Thursday | четверг |  |
| `day-5` | Friday | пятница |  |
| `day-6` | Saturday | суббота |  |
| `day-7` | Sunday | воскресенье |  |
| `date-full` | {day}, {month} {d}, {y} | {day}, {d} {month} {y} года |  |
| `key-backspace` | backspace | удалить назад |  |
| `key-tab` | tab | табуляция |  |
| `key-enter` | enter | ввод |  |
| `key-escape` | escape | выход |  |
| `key-space` | space | пробел |  |
| `key-home` | home | начало |  |
| `key-left` | left | влево |  |
| `key-up` | up | вверх |  |
| `key-right` | right | вправо |  |
| `key-down` | down | вниз |  |
| `key-page-up` | page up | страница вверх |  |
| `key-page-down` | page down | страница вниз |  |
| `key-end` | end | конец |  |
| `key-insert` | insert | вставка |  |
| `key-delete` | delete | удалить |  |
| `key-keypad-enter` | keypad enter | ввод на цифровой клавиатуре |  |
| `key-keypad-plus` | keypad plus | плюс на цифровой клавиатуре |  |
| `key-keypad-minus` | keypad minus | минус на цифровой клавиатуре |  |
| `key-keypad-multiply` | keypad multiply | умножить на цифровой клавиатуре |  |
| `key-keypad-divide` | keypad divide | разделить на цифровой клавиатуре |  |
| `key-shift` | shift | шифт | transliteration |
| `key-control` | control | контрол | transliteration |
| `key-alt` | alt | альт | transliteration |
| `key-super` | super | супер | transliteration |
| `key-caps-lock` | caps lock | кэпс лок | transliteration |
| `key-menu` | menu | меню |  |
| `key-f` | f {n} | эф {n} |  |
| `key-keypad` | keypad {key} | цифровая клавиатура {key} |  |
| `key-keypad-decimal` | decimal | точка |  |
| `key-keypad-other` | key | клавиша |  |
| `key-unknown` | key {code} | клавиша {code} |  |
| `echo-enter` | enter | ввод |  |
| `echo-backspace` | backspace | удалить назад |  |
| `echo-tab` | tab | табуляция |  |
| `echo-escape` | escape | выход |  |
| `echo-delete` | delete | удалить |  |
| `echo-f` | f {n} | эф {n} |  |

### text

| id | English | Russian | review note |
|---|---|---|---|
| `text-not-in-field` | not in a text field | не в текстовом поле |  |
| `text-not-available-mode` | not available in this mode | недоступно в этом режиме |  |
| `text-no-formatting` | no formatting information | нет сведений о форматировании |  |
| `text-formatting-unavailable` | formatting not available here | форматирование здесь недоступно |  |
| `text-no-selection` | no selection | нет выделения |  |
| `text-chars-selected` | {n} characters selected | выделено {n} {n#символ\|символа\|символов} |  |
| `live-updates-in` | {total} updates in {name} | {total} {total#обновление\|обновления\|обновлений} в {name} |  |
| `live-updates` | {total} updates | {total} {total#обновление\|обновления\|обновлений} |  |
| `focus-unknown` | unknown | неизвестно |  |
| `format-point` | {s} point | {s} пт |  |
| `format-bold` | bold | полужирный |  |
| `format-italic` | italic | курсив |  |
| `format-underline` | underline | подчёркнутый |  |
| `format-strike` | strikethrough | зачёркнутый |  |
| `format-superscript` | superscript | надстрочный |  |
| `format-subscript` | subscript | подстрочный |  |
| `format-colour-on` | {fg} on {bg} | {fg} на {bg} |  |
| `format-text-colour` | {c} text | {c} текст |  |
| `format-bg-colour` | {c} background | {c} фон |  |
| `format-aligned` | aligned {a} | выравнивание: {a} |  |
| `format-language` | language {l} | язык {l} |  |
| `format-align-left` | left | по левому краю |  |
| `format-align-right` | right | по правому краю |  |
| `format-align-center` | center | по центру |  |
| `format-align-justify` | justify | по ширине |  |
| `colour-black` | black | чёрный |  |
| `colour-white` | white | белый |  |
| `colour-red` | red | красный |  |
| `colour-green` | green | зелёный |  |
| `colour-blue` | blue | синий |  |
| `colour-yellow` | yellow | жёлтый |  |
| `colour-cyan` | cyan | голубой |  |
| `colour-magenta` | magenta | пурпурный |  |
| `colour-gray` | gray | серый |  |
| `colour-light-gray` | light gray | светло-серый |  |
| `colour-dark-red` | dark red | тёмно-красный |  |
| `colour-olive` | olive | оливковый |  |
| `colour-navy` | navy | тёмно-синий |  |
| `colour-purple` | purple | фиолетовый |  |
| `colour-teal` | teal | бирюзовый |  |
| `colour-lime` | lime | салатовый |  |
| `colour-orange` | orange | оранжевый |  |
| `colour-brown` | brown | коричневый |  |
| `colour-pink` | pink | розовый |  |
| `colour-unknown` | unknown | неизвестный |  |

### web

| id | English | Russian | review note |
|---|---|---|---|
| `web-mode-browse` | browse mode | режим просмотра |  |
| `web-mode-focus` | focus mode | режим фокуса |  |
| `web-visited-link` | visited link | посещённая ссылка |  |
| `web-heading-level` | heading level {n} | заголовок уровня {n} | order differs from NVDA |
| `web-graphic` | graphic | изображение |  |
| `web-landmark-banner` | banner | баннер |  |
| `web-landmark-main` | main | основное содержимое |  |
| `web-landmark-navigation` | navigation | навигация |  |
| `web-landmark-search` | search | поиск |  |
| `web-landmark-complementary` | complementary | дополнительное содержимое |  |
| `web-landmark-content-info` | content info | информация о странице |  |
| `web-landmark-form` | form | форма |  |
| `web-landmark-region` | region | область |  |
| `web-landmark` | {kind} landmark | ориентир: {kind} | order "ориентир: <kind>" |
| `web-list-with` | list with {n} {n#item\|items} | список из {n} {n#элемента\|элементов\|элементов} |  |
| `web-table-with` | table with {c} {c#column\|columns} and {r} {r#row\|rows} | таблица из {c} {c#столбца\|столбцов\|столбцов} и {r} {r#строки\|строк\|строк} |  |
| `web-block-quote` | block quote | цитата |  |
| `web-embedded-object` | embedded object | встроенный объект |  |
| `web-out-of-table` | out of table | вне таблицы |  |
| `web-out-of-list` | out of list | вне списка |  |
| `web-out-of-block-quote` | out of block quote | вне цитаты |  |
| `web-blank` | blank | пусто |  |
| `web-unlabeled` | unlabeled | без подписи |  |
| `web-frame` | frame | фрейм |  |
| `web-article` | article | статья |  |
| `web-list-box` | list box | список |  |
| `web-cant-move-focus` | can't move focus there | не удаётся переместить фокус |  |
| `web-loading` | loading | загрузка |  |
| `web-browse-unavailable` | browse mode is not available | режим просмотра недоступен |  |
| `web-not-in-page` | not in a web page | не веб-страница |  |
| `web-unselected` | unselected | не выделено |  |
| `web-all` | all | всё |  |
| `web-top` | top | верх |  |
| `web-bottom` | bottom | низ |  |
| `web-no-next-focusable` | no next focusable element | нет следующего элемента с фокусом |  |
| `web-no-prev-focusable` | no previous focusable element | нет предыдущего элемента с фокусом |  |
| `web-native-selection-web-only` | native selection is only available on web pages | системное выделение доступно только на веб-страницах |  |
| `web-native-selection-on` | native selection on | системное выделение включено |  |
| `web-native-selection-off` | native selection off | системное выделение выключено |  |
| `web-single-letter-on` | single letter navigation on | навигация одной буквой включена |  |
| `web-single-letter-off` | single letter navigation off | навигация одной буквой выключена |  |
| `web-no-container` | Not in a container | не в контейнере |  |
| `web-no-more-text-after-links` | no more text after a block of links | после блока ссылок нет текста |  |
| `web-not-in-cell` | Not in a table cell | не в ячейке таблицы |  |
| `web-table-edge` | Edge of table | край таблицы |  |

### webnav

| id | English | Russian | review note |
|---|---|---|---|
| `web-no-next-annotation` | no next annotation | нет следующей аннотации |  |
| `web-no-prev-annotation` | no previous annotation | нет предыдущей аннотации |  |
| `web-no-next-article` | no next article | нет следующей статьи |  |
| `web-no-prev-article` | no previous article | нет предыдущей статьи |  |
| `web-no-next-block-quote` | no next block quote | нет следующей цитаты |  |
| `web-no-prev-block-quote` | no previous block quote | нет предыдущей цитаты |  |
| `web-no-next-button` | no next button | нет следующей кнопки |  |
| `web-no-prev-button` | no previous button | нет предыдущей кнопки |  |
| `web-no-next-check-box` | no next check box | нет следующего флажка |  |
| `web-no-prev-check-box` | no previous check box | нет предыдущего флажка |  |
| `web-no-next-combo-box` | no next combo box | нет следующего комбинированного списка |  |
| `web-no-prev-combo-box` | no previous combo box | нет предыдущего комбинированного списка |  |
| `web-no-next-edit-field` | no next edit field | нет следующего поля ввода |  |
| `web-no-prev-edit-field` | no previous edit field | нет предыдущего поля ввода |  |
| `web-no-next-embedded-object` | no next embedded object | нет следующего встроенного объекта |  |
| `web-no-prev-embedded-object` | no previous embedded object | нет предыдущего встроенного объекта |  |
| `web-no-next-error` | no next error | нет следующей ошибки |  |
| `web-no-prev-error` | no previous error | нет предыдущей ошибки |  |
| `web-no-next-form-field` | no next form field | нет следующего поля формы |  |
| `web-no-prev-form-field` | no previous form field | нет предыдущего поля формы |  |
| `web-no-next-frame` | no next frame | нет следующего фрейма |  |
| `web-no-prev-frame` | no previous frame | нет предыдущего фрейма |  |
| `web-no-next-graphic` | no next graphic | нет следующего изображения |  |
| `web-no-prev-graphic` | no previous graphic | нет предыдущего изображения |  |
| `web-no-next-heading` | no next heading | нет следующего заголовка |  |
| `web-no-prev-heading` | no previous heading | нет предыдущего заголовка |  |
| `web-no-next-heading-1` | no next heading level 1 | нет следующего заголовка уровня 1 |  |
| `web-no-prev-heading-1` | no previous heading level 1 | нет предыдущего заголовка уровня 1 |  |
| `web-no-next-heading-2` | no next heading level 2 | нет следующего заголовка уровня 2 |  |
| `web-no-prev-heading-2` | no previous heading level 2 | нет предыдущего заголовка уровня 2 |  |
| `web-no-next-heading-3` | no next heading level 3 | нет следующего заголовка уровня 3 |  |
| `web-no-prev-heading-3` | no previous heading level 3 | нет предыдущего заголовка уровня 3 |  |
| `web-no-next-heading-4` | no next heading level 4 | нет следующего заголовка уровня 4 |  |
| `web-no-prev-heading-4` | no previous heading level 4 | нет предыдущего заголовка уровня 4 |  |
| `web-no-next-heading-5` | no next heading level 5 | нет следующего заголовка уровня 5 |  |
| `web-no-prev-heading-5` | no previous heading level 5 | нет предыдущего заголовка уровня 5 |  |
| `web-no-next-heading-6` | no next heading level 6 | нет следующего заголовка уровня 6 |  |
| `web-no-prev-heading-6` | no previous heading level 6 | нет предыдущего заголовка уровня 6 |  |
| `web-no-next-landmark` | no next landmark | нет следующего ориентира |  |
| `web-no-prev-landmark` | no previous landmark | нет предыдущего ориентира |  |
| `web-no-next-link` | no next link | нет следующей ссылки |  |
| `web-no-prev-link` | no previous link | нет предыдущей ссылки |  |
| `web-no-next-list` | no next list | нет следующего списка |  |
| `web-no-prev-list` | no previous list | нет предыдущего списка |  |
| `web-no-next-list-box` | no next list box | нет следующего списка |  |
| `web-no-prev-list-box` | no previous list box | нет предыдущего списка |  |
| `web-no-next-list-item` | no next list item | нет следующего элемента списка |  |
| `web-no-prev-list-item` | no previous list item | нет предыдущего элемента списка |  |
| `web-no-next-paragraph` | no next paragraph | нет следующего абзаца |  |
| `web-no-prev-paragraph` | no previous paragraph | нет предыдущего абзаца |  |
| `web-no-next-radio-button` | no next radio button | нет следующей радиокнопки |  |
| `web-no-prev-radio-button` | no previous radio button | нет предыдущей радиокнопки |  |
| `web-no-next-separator` | no next separator | нет следующего разделителя |  |
| `web-no-prev-separator` | no previous separator | нет предыдущего разделителя |  |
| `web-no-next-table` | no next table | нет следующей таблицы |  |
| `web-no-prev-table` | no previous table | нет предыдущей таблицы |  |
| `web-no-next-unvisited-link` | no next unvisited link | нет следующей непосещённой ссылки |  |
| `web-no-prev-unvisited-link` | no previous unvisited link | нет предыдущей непосещённой ссылки |  |
| `web-no-next-visited-link` | no next visited link | нет следующей посещённой ссылки |  |
| `web-no-prev-visited-link` | no previous visited link | нет предыдущей посещённой ссылки |  |

### words

| id | English | Russian | review note |
|---|---|---|---|
| `word-blank` | blank | пусто |  |
| `word-object` | object | объект |  |
| `word-cap` | cap | заглавная |  |
| `level-none` | none | без пунктуации |  |
| `level-some` | some | немного |  |
| `level-most` | most | большинство |  |
| `level-all` | all | вся |  |
| `word-confirm` | punctuation {level} | пунктуация {level} |  |
| `lock-caps` | Caps Lock | Caps Lock | kept in Latin |
| `lock-num` | Num Lock | Num Lock | kept in Latin |
| `lock-scroll` | Scroll Lock | Scroll Lock | kept in Latin |
| `lock-on` | {key} on | {key} включён |  |
| `lock-off` | {key} off | {key} выключен |  |
| `history-secret` | password | пароль |  |
| `history-empty` | no speech history | история речи пуста |  |
| `history-oldest` | oldest | самое старое |  |
| `history-newest` | newest | самое новое |  |
| `sleep-on` | sleep mode on | режим сна включён |  |
| `sleep-off` | sleep mode off | режим сна выключен |  |
| `battery-none` | no battery | батареи нет |  |
| `battery-unavailable` | battery status unavailable | состояние батареи недоступно |  |
| `battery-percent` | {n} percent | {n} {n#процент\|процента\|процентов} |  |
| `battery-charging` | charging | заряжается |  |
| `battery-discharging` | discharging | разряжается |  |
| `battery-full` | fully charged | полностью заряжена |  |
| `battery-empty` | empty | разряжена |  |
| `battery-pending` | waiting to charge | ожидает зарядки |  |
| `battery-time` | {dur} remaining | осталось {dur} |  |
| `battery-hm` | {h} h {m} min | {h} ч {m} мин |  |
| `battery-m` | {m} min | {m} мин |  |

