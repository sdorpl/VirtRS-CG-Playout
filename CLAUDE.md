# VirtRS CG Playout

Sidecar (Node.js – najnowszy, minimum 22 – + bezgłowy Chromium) łączący **ErsatzTV** z silnikiem grafiki **VizRS**: komendy TCP na start klipu,
reklamy bez grafiki i wtapianie `/output` przez element grafiki „script” ErsatzTV. ErsatzTV zostaje niezmienione.

Rodzina VizRS (repozytoria obok, `../`):
- **VizRS CG System** (`../VizRS CG System`) – silnik grafiki (Rust): Artist, Trio, `/output`, API TCP 6100 / HTTP / SSE. Plan kanału: `docs/plany/PLAN-kanal.md`.
- **VizRS ffPlayout System** (`../VizRS ffPlayout System`) – **archiwum od 2026-10-24**: porzucony własny playout 24/7 (fork ffplayout, GPL-3.0). Zostaje jako wzorzec integracji VizRS, nie jest rozwijany.
- **VirtRS CG Playout** (to repo) – ta integracja dla ErsatzTV.

Język pracy z użytkownikiem i dokumentacji: **polski**. Kod, komentarze i komunikaty logu są po angielsku — pisz zgodnie z otoczeniem.

## Zacznij każdą sesję tutaj

1. `git log -5`, `git status`.
2. Przy zmianie zachowania przeczytaj właściwą sekcję [docs/DOKUMENTACJA.md](docs/DOKUMENTACJA.md) — w tym samym commicie ją zaktualizujesz.
3. Przy komendach VizRS sprawdź składnię w `../VizRS CG System/docs/API.md` (sekcja TCP).

## Niepodważalne zasady

- **Z VizRS tylko przez protokół**: TCP 6100 (`AUTH`, `STATE`, `TAKE`, `OUT`, …) i strona `/output`. Nie kopiuj kodu między repozytoriami rodziny (różne licencje; ffPlayout jest GPL-3.0).
- **Wzorzec VizRS ffPlayout System (archiwum)**: opcje (`on_clip`, `on_ad`, `output_url`, adres/token), symbole zastępcze i zachowanie reklam pochodzą z ffPlayout i zostają zgodne, żeby szablony komend dało się przenieść. ffPlayout nie jest już rozwijany: nowe funkcje projektuj tutaj, niczego tam nie dopasowuj (tabela w DOKUMENTACJA §2 to mapa pojęć).
- **ErsatzTV bez zmian**: integracja tylko przez element „script” (`etv/templates/graphics-elements/script/vizrs.yml`) i klienta `client/vizrs-etv-client.py` (sama biblioteka standardowa Pythona – obraz ErsatzTV nie ma nic więcej).
- **Grafika nigdy nie zatrzymuje emisji**: komendy w tle z limitami czasu, błędy tylko do logu, awaria Chromium = czyste klatki i restart, brak sidecara = klip bez grafiki.
- **Bez GPU na ścieżce grafiki**: Chromium renderuje programowo; docelowo także Oracle Cloud Ampere A1 (arm64) – pamiętaj o budżecie CPU. Obraz buduje się na amd64 i arm64.
- **Mało zależności**: jedyna zależność runtime to `sharp` (wersja przypięta). Nowej nie dodawaj bez potrzeby.
- **Gdzie co opisywać**: README tylko wprowadza i odsyła; opis działania, opcje i zmienne środowiskowe tylko w `docs/DOKUMENTACJA.md`. Nowa opcja = `src/config.js` + `config.example.json` (gdy dotyczy) + `docker-compose.yml` + DOKUMENTACJA §5 + test.

## Uruchamianie

```bash
docker compose up -d                       # VizRS :8788, sidecar :7700, ErsatzTV :8409
node bin/virtrs-cg.js config.json          # bez Dockera (Node 22+, Chromium w PATH albo VIZRS_BROWSER)
```

Nakładki: `docker-compose.ampere.yml` (Oracle Ampere A1), `docker-compose.nvidia.yml` (NVENC w ErsatzTV).

## Testy (bramka przed commitem)

```bash
npm test                                   # node --test test/*.test.mjs
```

`units` (szablony, pakiety, konfiguracja), `integration` (udawany VizRS i klient ErsatzTV), `chromium` (prawdziwe przechwytywanie; pomijane bez przeglądarki).

Po testach na stosie Dockera zawsze sprzątaj to, co powstało na potrzeby testu (kontenery, obrazy, wolumeny, pliki), ale **dane działających instancji tego projektu zostawiaj** – bez `down -v` i `volume prune`. Szczegóły: DOKUMENTACJA §7.

## Struktura

| Ścieżka | Rola |
|---|---|
| `bin/virtrs-cg.js` | start: konfiguracja, serwer, zamykanie |
| `src/config.js` | domyślne opcje, plik JSON, zmienne środowiskowe |
| `src/server.js` | HTTP: `POST /etv?channel=N` (strumień pakietów), `GET /health`; przechwytywanie na rozmiar, zdarzenia na kanał |
| `src/template.js` | kontekst klipu z danych szablonu ErsatzTV, symbole `{title}`, `{next_*}`…, wykrywanie reklam |
| `src/vizrs.js` | klient TCP VizRS (`AUTH`, `STATE`, komendy), kolejka zdarzeń kanału, zdejmowanie/przywracanie stron przy reklamach |
| `src/capture.js` | bezgłowy Chromium (DevTools screencast) renderujący `/output`, restart po awarii |
| `src/png.js`, `src/packets.js`, `src/stream.js` | dekodowanie PNG (sharp), pakiety ETV (`Rectangles`/`Repeat`), tempo strumienia i `lead_seconds` |
| `client/vizrs-etv-client.py` | klient w kontenerze ErsatzTV: stdin → sidecar → stdout |
| `etv/templates/…/vizrs.yml` | definicja elementu grafiki „VizRS” dla ErsatzTV |
| `Dockerfile` | obraz sidecara (używany przez compose) |
| `docker/sidecar.Dockerfile` | starsza wersja obrazu, nieużywana przez compose |
| `test/` | testy `node --test` |
| `seed/` | dane startowe stosu: show VizRS, baza ErsatzTV (kanał 1), media testowe; `init.sh` kopiuje je do pustych wolumenów (DOKUMENTACJA §4) |
| `bench/` | benchmark przechwytywania: sidecar, Chromium zsynchronizowany z klatkami, WPE (osobny obraz i compose, DOKUMENTACJA §8) |
| `.github/workflows/ci.yml` | testy; obraz amd64 + arm64 do ghcr po znaczniku `v*` |

## Commity

- Jedna spójna zmiana = jeden commit, po polsku, opisowo. Tylko zielone stany.
- Pliki dodawaj jawnie (`git add ścieżka`). Nie commituj `config.json`, `media/` (własne media; testowe są w `seed/media/`), zrzutów PNG ani logów. W `seed/` nigdy sekretów ErsatzTV (`*-secrets.json`, `data-protection`) ani tokenów.
- Commit i push tylko na prośbę użytkownika.
