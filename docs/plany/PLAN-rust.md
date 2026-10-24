# PLAN: migracja sidecara VirtRS CG z Node.js do Rusta

> Status (2026-10-25): **etapy 0–2 zrobione** – gałąź `rust-edit`, testy kontraktu (czarna skrzynka z `VIRTRS_CG_BIN`, wektory `test/vectors/`), szkielet sidecara w Ruście: konfiguracja zgodna z wektorami, log, HTTP, `/health`. Następny: etap 3 (komendy VizRS i `/etv` bez grafiki).
> Gałąź robocza: **`rust-edit`**. `main` do etapu 8 zostaje wydawalny na Node.js.
> Powiązane: [DOKUMENTACJA.md](../DOKUMENTACJA.md) (opis działania – kontrakt, którego migracja nie zmienia).

## 0. Cel

Sidecar (`bin/` + `src/*.js`, ok. 1700 linii, zależność `sharp`) staje się jednym plikiem binarnym w Ruście, tak jak silnik VizRS CG System. Zyski:

- mniej CPU i pamięci obok Chromium, co ma znaczenie na Oracle Ampere A1 (dziś Node ma ok. 60–80 MB RSS, cel to ok. 10 MB);
- znika natywna zależność libvips (`sharp`) i cały runtime Node w obrazie;
- przetwarzanie klatek (`visibleRects`, `yuva420p`, skalowanie) w kodzie kompilowanym zamiast pętli JS. Dla nakładki ffmpeg przy 50 kl./s i 1080p to najdroższy fragment poza Chromium.

## 1. Kontrakt – bez zmian

| Element | Gdzie opisany |
|---|---|
| `POST /etv?channel=N[&render=…]`, `GET /ffmpeg?channel=N[&wait=ms]`, `GET /ffmpeg/frames?session=N[&fps=…]`, `GET /health` | DOKUMENTACJA §1, `src/server.js` |
| pakiety ETVG (`Clear`, `Repeat`, `Rectangles`) i klatki `yuva420p` – bajt w bajt | `src/packets.js`, `src/yuva.js` |
| opcje `config.json`, zmienne środowiskowe (`VIZRS_ON_*` z `\n`), domyślne wartości | DOKUMENTACJA §5, `src/config.js` |
| symbole zastępcze, wykrywanie reklam, zdejmowanie i przywracanie stron | DOKUMENTACJA §3, `src/template.js`, `src/vizrs.js` |
| format logu (`<ISO> INFO …`, `DEBUG=1`) | `bin/virtrs-cg.js` |
| `client/*.py`, `etv/…/vizrs.yml`, `seed/`, compose (poza obrazem sidecara) | bez zmian – działają w kontenerze ErsatzTV |

## 2. Decyzje techniczne

| Obszar | Node.js | Rust | Uzasadnienie |
|---|---|---|---|
| współbieżność | pętla zdarzeń | `std::thread`, `mpsc`, `Condvar`; bez tokio | kilka kanałów i sesji; blokujący zapis daje backpressure |
| HTTP | `node:http` | własny minimalny HTTP/1.1 na `TcpListener` | 4 trasy, strumienie bez końca: odpowiedź bez `Content-Length`, `Connection: close` (nakładka pyta HTTP/1.0), nagłówki wysyłane od razu |
| DevTools | WebSocket + `DevToolsActivePort` | `--remote-debugging-pipe` (fd 3/4, JSON zakończony `\0`) | bez klienta WebSocket i odpytywania pliku |
| koniec Chromium | `SIGKILL` przy zamykaniu | `PR_SET_PDEATHSIG` w `pre_exec` | przeglądarka kończy się z sidecarem bez obsługi sygnałów |
| PNG | `sharp` (libvips) | crate `png` | czysty Rust, szybki inflate (fdeflate) |
| skalowanie `render_size` | `sharp`, Lanczos z premultiplikacją | własny rozdzielny Lanczos3 z premultiplikacją | znika ostatnia zależność natywna |
| `ad_match` | `RegExp(…, "i")` | crate `regex-lite` | mały, bez zależności |
| JSON | wbudowany | własny moduł (parsowanie + zapis) | jak w VizRS CG System, ale napisany od nowa – bez kopiowania kodu |
| base64 | `Buffer` | własna funkcja | ok. 20 linii |

Zależności runtime: **`png` + `regex-lite`** (wersje przypięte w `Cargo.lock`). Zasada „Mało zależności” w CLAUDE.md zmienia się przy scaleniu.

Model wątków:
- **kanał** (`ChannelEvents`): wątek z kolejką `mpsc` w miejsce łańcucha `Promise` – ta sama kolejność komend; przerwa reklamowa (`GAP_GRACE`) przez `recv_timeout`;
- **przechwytywanie**: wątek czytający rurę DevTools i wątek dekodujący. Skrzynka trzyma tylko najnowszy PNG (jak `pendingPng`). Historia to `Mutex<VecDeque<Arc<Frame>>>`, pakiet i `yuva` w `OnceLock` na klatce;
- **sesja**: wątek na połączenie, tempo z `Instant`; wstrzymanie i wznowienie (przejęcie przez nakładkę) przez `Condvar`.

## 3. Testy: najpierw kontrakt

1. **Czarna skrzynka.** `test/integration.test.mjs` uruchamia binarkę z `VIRTRS_CG_BIN` (domyślnie `node bin/virtrs-cg.js`) zamiast wołać `createServer()` w procesie. Rozmiary renderowania i klatkaż sprawdza przez `/health` zamiast atrapy przechwytywania. Przeglądarką jest `/bin/false`, więc klatki są przezroczyste. Treść grafiki sprawdza `chromium.test.mjs` na prawdziwym Chromium: pakiety ETV i `yuva420p` przez proces sidecara. Wymagania wobec budowy Rusta: `/health` z polami `sessions`, `captures[].size`, `render`, `fps`, `status`, `seq`; ukryta opcja `gap_grace_ms` w pliku konfiguracji; nieudany start przeglądarki = przezroczyste klatki i ponowna próba.
2. **Wektory referencyjne.** `test/vectors/generate.mjs` zapisuje `test/vectors/*.json` z obecnej implementacji: `template` (kontekst klipu, komendy, TimeSpan, zegar, reklamy), `vizrs` (`onAir`), `config` (domyślne, plik, zmienne), `server` (`parseRate`, `renderSize`, `everyNthFrame`), `frames` (prostokąty, pakiety, `yuva`), `png`, `scale`. Obrazy są opisane wypełnieniami (`paint`), więc Rust buduje to samo RGBA. `cargo test` porównuje bajt w bajt, a skalowanie z tolerancją ±2 na kanał.
3. **CI:** `cargo test`, potem `npm test` z `VIRTRS_CG_BIN=rust/target/release/virtrs-cg`.

## 4. Etapy

Każdy etap kończy się zielonym commitem na `rust-edit`.

| # | Etap | Zakres | Stan |
|---|---|---|---|
| 0 | Przygotowanie | praca na `main` zacommitowana, gałąź `rust-edit`, crate w `rust/` (bo `src/` zajmuje JS), job `rust` w CI | ✅ |
| 1 | Kontrakt | testy czarnej skrzynki (`test/sidecar.mjs`, przeglądarka `/bin/false` = przezroczyste klatki; treść grafiki w `chromium.test.mjs`) i wektory `test/vectors/*.json` z generatorem, nadal na JS | ✅ |
| 2 | Szkielet | `json.rs` (parsowanie i zapis jak `JSON.stringify`), `config.rs` (plik + zmienne, zgodny z `test/vectors/config.json`), `log.rs`, `http.rs` (jedno żądanie na połączenie, wątek na połączenie, `Content-Length` lub chunked, limit 4 MB), `server.rs` z `/health`, `main.rs` (argument `config.json`/`CONFIG`); pozostałe trasy na razie 404 | ✅ |
| 3 | Logika bez grafiki | `template.rs` (daty ISO z offsetem i 7 cyframi ułamka z .NET), `vizrs.rs` (TCP z limitami 2 s, `AUTH`/`STATE`/`OUT`/`TAKE`, reklamy, `GAP_GRACE`), `/etv` z pustymi klatkami | |
| 4 | Klatki | `packets.rs`, `png.rs`, `rects.rs`, `yuva.rs`, `scale.rs`, `stream.rs` (wyprzedzenie, `SAMPLE_DELAY_MS`, `Repeat`); cecha `Source` z atrapą do testów | |
| 5 | Chromium | `capture.rs`: te same flagi, CDP przez rurę, screencast z `everyNthFrame`, zrzut po 150 ms ciszy, czas z `metadata.timestamp`, pauza po 30 s, `raiseFps`, restart po 3 s z czystymi klatkami | |
| 6 | Nakładka ffmpeg | `/ffmpeg`, `/ffmpeg/frames`, `HOLD_MS`, `FRAMES_WAIT_MS`, kanały z nakładką | |
| 7 | Obraz i CI | wieloetapowy `Dockerfile`, arm64 kompilacją krzyżową (`aarch64-unknown-linux-gnu`) zamiast QEMU, `ci.yml`, compose | |
| 8 | Odbiór | pełny stos (oba tory grafiki, reklamy, restart Chromium, brak sidecara); benchmark §8 JS vs Rust na amd64 i Ampere A1 – nie gorzej niż JS | |
| 9 | Sprzątanie | usunięcie `bin/`, `src/*.js`, `docker/sidecar.Dockerfile`; `rust/` do korzenia; DOKUMENTACJA, README, CLAUDE.md; scalenie do `main`, wersja zgodna z VizRS | |

## 5. Zamierzone różnice względem Node.js

Na scaleniu trafią do DOKUMENTACJI §5.

- Nieznana opcja w pliku konfiguracji daje ostrzeżenie w logu (Node pomijał ją po cichu).
- Zły typ opcji w pliku (np. `"no_sandbox": "yes"`) zatrzymuje start z komunikatem, która opcja jest zła. Node uruchamiał się i działał dalej z dziwną wartością. Liczba zapisana jako tekst (`"lead_seconds": "6"`) jest przyjmowana.
- `LEAD_SECONDS`, który nie jest liczbą, daje ostrzeżenie i zostawia poprzednią wartość (Node przyjmował `NaN`).
- Nieprawidłowy wzorzec `ad_match` ma dawać ostrzeżenie i „nie reklama”. W Node wyjątek wywraca proces.

## 6. Wzorce z VizRS ffPlayout System (archiwum, GPL-3.0)

Wolno brać tylko pomysły i zachowanie, kodu nie (CLAUDE.md, „Niepodważalne zasady”). ffPlayout używa tokio i tungstenite, więc architektura i tak jest inna.

- `crates/app/src/utils/vizrs.rs` – kontekst klipu, symbole, `onAir`, wysyłanie komend: wzorzec dla etapu 3.
- `crates/app/src/utils/vizrs_capture.rs` – Chromium przez DevTools (WebSocket), dekodowanie w osobnym wątku, `every_nth_frame`: wzorzec dla etapu 5. W razie problemów z trybem rury to także zapasowa ścieżka WebSocket.

## 7. Ryzyka

- **`ad_match`.** `regex-lite` nie ma lookahead ani odwołań wstecznych. Nieprawidłowy wzorzec ma dawać ostrzeżenie w logu i „nie reklama”, nigdy błąd. Trzeba to opisać w DOKUMENTACJI §5.
- **Daty.** Własny parser zastępuje `Date.parse`. Wektory zbieram z prawdziwych danych szablonu z `seed/`.
- **Sesja wstrzymana.** Nagłówki muszą wyjść od razu (dziś robi to `flushHeaders`), inaczej klient ETV czeka.
- **Skalowanie.** Własny Lanczos odbiega od libvips o pojedyncze wartości. Test ma tolerancję.
- **Tryb rury DevTools.** Do potwierdzenia na `chromium-headless-shell` z Debiana (amd64 i arm64) w etapie 5. Zapasowo własny klient WebSocket.
