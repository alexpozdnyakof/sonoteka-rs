# sonoteka-rs — план разработки

## Контекст стенда (чтобы не вспоминать)

- Сервер: MacBook Pro (Intel, macOS 15.7.3), `~/sonoteka/`, launchd-агент `ru.sonoteka.rs`, лог `~/sonoteka/server.log`.
- Библиотека: `/Volumes/Macintosh HD 1/music` — 563 трека / 67 альбомов, внешний HDD.
- Деплой с MBA: `./deploy.sh` (кросс-сборка x86_64 → scp → launchctl kickstart → ждёт свежий `listening`).
- Доступ клиентов: дома — LAN `http://192.168.88.137:4533` (быстро), вне дома — меш `http://100.64.0.5:4533`.
- Auth: пользователь `admin`, пароль в `~/sonoteka/sonoteka.env`.
- ID = md5 от ключа (путь/имя): стабильны между ресcанами, ломаются при ретеге/переносе (ledger — в фазе 3).

## Сделано

- [x] Скелет: сканер тегов (lofty), SQLite, авторизация md5+sha256, stream с HTTP Range, браузинг библиотеки
- [x] Обложки: извлечение встроенных + диск-кэш
- [x] Feishin-дожим: getScanStatus, getSimilarSongs2, getTopSongs, getOpenSubsonicExtensions (анонимно)
- [x] Инфра: launchd + start.sh (ожидание тома) + deploy.sh
- [x] Лог запросов `[req]` в server.log
- [x] **Жанры**: getGenres, getSongsByGenre, byGenre в getAlbumList/getAlbumList2
- [x] **Плейлисты**: полный CRUD + импорт .m3u/.m3u8 (id от пути файла, реимпорт in-place). В библиотеке 5 m3u — подхватились сами
- [x] **Scrobble → БД**: play_count/played_at из scrobble, playCount в child, честные getTopSongs
- [x] **Обложки 500px**: ресайз через crate image (jpeg q85), кэш `art-<id>-500.`
- [x] **Ручной rescan**: `/rest/scan` + `startScan` без рестарта, флаг scanning в getScanStatus
- [x] **Движок правил** `radio/`: JSON-правила (жанры, артисты, эра, порядок random/newest/top, exclude_recent_days)
- [x] **Умные плейлисты**: rules в колонке playlists.rules, автообновление при скане и при смене правил
- [x] **Эфир** `/radio`: общая очередь → broadcast, ffmpeg `-re` (realtime) в Opus 96k Ogg; ICY StreamTitle только по `Icy-MetaData: 1`, иначе чистый Ogg. Параметры очереди `?genres=..&order=..&limit=..&exclude_recent_days=..`. ffmpeg 6.1.1 статикой в `~/sonoteka/ffmpeg`, путь через `SONOTEKA_FFMPEG`
- [x] **TCC-лаунчер**: стабильный бинарь `sonoteka-rs-launcher` (не меняется между деплоями) exec'ит `sonoteka-server` — разрешение «Съёмные тома» выдано один раз и переживает редеплои
- [x] **Натуральная сортировка**: коллация `NATURAL` в rusqlite (цифровые блоки сравниваются численно), `768 ...` больше не всплывает наверх
- [x] **Нормализация жанров**: регистр при скане (первая буква заглавная), дубликаты схлопнулись
- [x] **Транскодинг в /stream**: `format=opus|mp3` и `maxBitRate` (для lossless); mp3-источник отдаётся как есть, `download` — всегда оригинал, Range на прямых потоках жив
- [x] **Snapshot-тесты** (insta): child/album/playlist/envelope — регрессии конвертов ловятся сами
- [x] **Gapless-эфир**: батч кодируется одним ffmpeg через concat-лист — стыки sample-continuous (один OpusHead на батч), NowPlaying ведётся таймером по длительностям тегов

## Фаза 2 — радио (остаток)

- [ ] InternetRadioStations CRUD (закладки на внешние станции) — низкий приоритет

## Фаза 3 — совместимость и качество

- [ ] XML-конверт ответов (legacy-клиенты без JSON)
- [ ] apiKey auth + constant-time сравнение токенов
- [ ] Identity ledger: ID не портятся при ретеге/переносе (долговечность плейлистов)
- [ ] Мульти-библиотека: несколько `SONOTEKA_MUSIC_FOLDER`

## Фаза 4 — расширения

- [ ] Скробблинг наружу: ListenBrainz / Last.fm

## Внешние блокеры

- [ ] awg-туннель ~10 КБ/с между пирами — расследование запущено у агента awg. Меш-стриминг и «эфир вне дома» ждут его результата.
- [x] **macOS TCC и деплой** — РЕШЕНО лаунчером (см. «Сделано»). Симптом был: после деплоя нет свежего `library scan complete`, процесс жив, сокетов нет, main-thread в `open()`. Если вернётся — смотреть ответственный процесс через `sample`.
