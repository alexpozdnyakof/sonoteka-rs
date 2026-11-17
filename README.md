# sonoteka-rs

Self-contained [Subsonic](https://www.subsonic.org/)-compatible music server
written in Rust. Scans your music folder (FLAC, MP3, OGG, ...) from disk,
indexes tags into one SQLite database and serves the library over the
Subsonic API (v1.16.1) — a single static binary, no Plex, no backend
services, no bundled web player. Bring your own Subsonic client: Amperfy or
Substreamer (iOS), Symfonium or Ultrasonic (Android), Feishin or Supersonic
(macOS/Linux/Windows).

A personal, own-your-library alternative to Navidrome you can read end to end.

## Запуск

```sh
export SONOTEKA_MUSIC_FOLDER=~/Music/flac   # папка с библиотекой
export SONOTEKA_DATA_FOLDER=./data          # SQLite-база
export SONOTEKA_PASSWORD=secret123          # единственный пользователь
export SONOTEKA_PORT=4533                   # по умолчанию 4533
cargo run --release
```

Бинарник слушает на `0.0.0.0:4533`. Внутри awg-меша достаточно обращаться
к `http://<device>.awg:4533` — TLS не нужен, трафик и так в туннеле.

## Проверка

```sh
T=$(md5 -s "secret123salt42" | sed 's/.*= //')
curl -s "http://127.0.0.1:4533/rest/ping?u=admin&t=$T&s=salt42&v=1.16.1&c=cli&f=json"
curl -s "http://127.0.0.1:4533/rest/getArtists?u=admin&t=$T&s=salt42&v=1.16.1&c=cli&f=json"
```

## Проекты, на которые ориентируемся

| | rs-subsonic | subsonic-rs (Spookdot) | sonoteka-rs (мы) |
|---|---|---|---|
| Что это | OpenSubsonic-фасад **над Plex** | клиентская библиотека | сервер, владеющий библиотекой |
| Файлы сканирует | Plex | — | сам (lofty) |
| Форма | workspace из 10 крейтов | WIP, без лицензии | один крейт |

## Устройство

| Модуль | Ответственность |
|---|---|
| `src/db.rs` | схема SQLite, upsert артистов/альбомов/треков, запросы |
| `src/scan.rs` | обход папки, чтение тегов (lofty), группировка альбомов |
| `src/api/` | диспетчер `/rest/{*method}`, auth, конверт ответов |
| `src/stream.rs` | заготовка под `stream`/`getCoverArt` |

Принятые решения (прочитай перед изменениями):

- **Полный rescan при старте** вместо инкрементального: библиотека локальная
  и маленькая, wipe-and-rebuild проще и не оставляет мусора.
- **Альбом группируется по album artist** (падает на artist), трек хранит
  своего артиста — компиляции собираются в один альбом.
- **ID = md5 от доменного ключа** (`artist:<name>`, `album:<artist>:<name>`,
  `track:<path>`), как в Navidrome. Стабильны между rescans.
- **Только JSON** (`f=json`). Мобильные клиенты его едят; XML-конверт —
  отдельная задача.
- Один пользователь, auth по спецификации Subsonic: `t`=md5(pass+salt), `s`,
  либо legacy `p`=hex(pass). TODO: OpenSubsonic apiKey, constant-time сравнение.

## Дорожная карта

Эндпоинты, без которых сервер «не играет» в клиентах (в порядке работ):

- [x] `stream` с корректным HTTP Range (suffix/open-ended/middle/416)
- [x] `getAlbumList2`, `getAlbum`, `getSong` — браузинг библиотеки
- [x] `getIndexes`/`getMusicDirectory` — дерево по тегам (как у Navidrome)
- [x] `getArtist`, `getAlbumList` (legacy), `search3`
- [x] `getCoverArt` — извлечение встроенной картинки + диск-кэш
- [x] `getScanStatus`, `getSimilarSongs2`, `getTopSongs`, `getOpenSubsonicExtensions`
- [x] `getMusicFolders`, `getStarred2`, `getArtistInfo2`, `scrobble` (заглушки)
- [x] авторизация md5 **и sha256** (OpenSubsonic-клиенты, Feishin)
- [ ] `scrobble` сохранять: play_count / played_at
- [ ] плейлисты: `getPlaylists`, `createPlaylist`, `updatePlaylist`, `deletePlaylist`
- [ ] импорт `.m3u` из папки библиотеки
- [ ] snapshot-тесты конвертов ответов (insta)
- [ ] XML-конверт ответов (для клиентов без JSON)
- [ ] мульти-библиотека (несколько `SONOTEKA_MUSIC_FOLDER`)
- [ ] транскодинг через ffmpeg (Opus для мобильного интернета)

## Эксплуатация на MacBook Pro

Сервис живёт в launchd: `~/Library/LaunchAgents/ru.sonoteka.rs.plist` →
`~/sonoteka/start.sh` (ждёт монтирование тома с музыкой) → `sonoteka-rs`.
Env: `~/sonoteka/sonoteka.env`. Лог: `~/sonoteka/server.log`.

```sh
./deploy.sh   # из репозитория на MBA: сборка → scp → restart → проверка
ssh macbookpro 'launchctl kickstart -k gui/$(id -u)/ru.sonoteka.rs'  # рестарт
```

## Топики для GitHub-репозитория

`subsonic`, `opensubsonic`, `music-server`, `self-hosted`, `rust`, `flac`,
`audio-streaming`, `navidrome-alternative`
