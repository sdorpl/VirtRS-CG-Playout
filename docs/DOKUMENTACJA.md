# VirtRS CG Playout – dokumentacja

1. [Jak to działa](#1-jak-to-działa)
2. [Odpowiedniki w VizRS ffPlayout System](#2-odpowiedniki-w-vizrs-ffplayout-system)
3. [Komendy i symbole zastępcze](#3-komendy-i-symbole-zastępcze)
4. [Uruchamianie w Dockerze](#4-uruchamianie-w-dockerze)
5. [Uruchamianie bez Dockera, konfiguracja](#5-uruchamianie-bez-dockera-konfiguracja)
6. [Awarie](#6-awarie)
7. [Testy](#7-testy)
8. [Benchmark silników: Chromium a WPE WebKit](#8-benchmark-silników-chromium-a-wpe-webkit)

## 1. Jak to działa

```
ErsatzTV ── element grafiki „script” ──► vizrs-etv-client.py ──HTTP──► virtrs-cg (sidecar)
  ▲  stdin: dane szablonu (JSON)                                         │  TCP 6100 ─► VizRS
  └──────── stdout: pakiety grafiki ETV ◄────────────────────────────────┘  Chromium ◄─ /output
```

ErsatzTV uruchamia element „script” dla każdej pozycji playoutu, podaje na
stdin dane szablonu (ścieżka, tytuł, start/stop, wpisy EPG, rozdzielczość,
klatkaż) i nakłada surowe klatki czytane ze stdout (`format: packet`).
Klient w Pythonie (sama biblioteka standardowa) tylko przekazuje dane w obie
strony, więc w sidecarze działa jedna przeglądarka na rozmiar kanału (i rozmiar
renderowania, niżej), także między pozycjami.

### Rozdzielczość warstwy grafiki

ErsatzTV zawsze nakłada grafikę na płótno w rozmiarze kanału. Domyślnie
Chromium renderuje `/output` w tym samym rozmiarze. Opcja `render_size`
(`VIZRS_RENDER_SIZE`) albo parametr `render` w adresie elementu
(`http://cg:7700/etv?channel=1&render=720p`, ma pierwszeństwo) każe
renderować w innym rozmiarze, a sidecar skaluje widoczne prostokąty do
rozmiaru kanału (libvips przez `sharp`, filtr Lanczos, z premultiplikacją
alfy). Skalowane są tylko widoczne prostokąty, nie cała klatka.

| Wartość | Rozmiar renderowania |
|---|---|
| puste, `auto` | rozmiar kanału (bez skalowania) |
| `720p`, `720`, `540p`… | ta wysokość w proporcjach kanału (1080p → 1280×720) |
| `1280x720` | dokładnie ten rozmiar (zachowaj proporcje kanału, inaczej obraz się rozciągnie) |

Mniejszy rozmiar renderowania oznacza mniej pracy Chromium przy każdej
zmienionej klatce kosztem ostrości tekstu, a większy niż kanał daje
supersampling. Na grafikę statyczną nie ma wpływu, bo nie generuje klatek.
Pomiar na x86 dla kanału 1080p i animacji ciągłej (przewijany ticker,
mrugające „Na żywo”), CPU Chromium i sidecara na jedną klatkę na sekundę:
1080p ok. 7,8%, `720p` ok. 5,6% (−30%), `540p` ok. 4,3% (−45%).
Błędna wartość trafia do logu, a grafika renderuje się w rozmiarze kanału.
Bez `sharp` (instalacja bez zależności) opcja jest pomijana.
Kanały o tym samym rozmiarze i rozmiarze renderowania dzielą jedną przeglądarkę.

Sidecar (Node.js, w obrazie i CI najnowszy – `node:current`; minimum 22):

| Adres | Co |
|---|---|
| `POST /etv?channel=1[&render=720p]` | treść: dane szablonu ErsatzTV; odpowiedź: niekończący się strumień pakietów ETV; `render` – rozmiar renderowania grafiki (§1) |
| `GET /health` | liczba sesji i stan przechwytywania (`size` kanału, `render`, `status`: `capturing`, `paused`, `restarting`…, `seq`) |

Chromium wysyła PNG tylko przy zmianie strony. Sidecar dekoduje najnowszą
klatkę, przycina ją do widocznych obszarów i wysyła jako jeden pakiet
`Rectangles` z osobnym prostokątem na każdą grupę grafik (np. logo w rogu
i pasek na dole to dwa małe prostokąty, a nie jeden na cały ekran;
przerwy węższe niż 32 px zostają w jednym prostokącie). Niezmienione klatki idą jako `Repeat`, więc statyczna grafika
prawie nic nie kosztuje. Animacje pod obciążeniem gubią klatki, ale nie
zostają w tyle.

Płynność animacji (rozwiązania z VizRS ffPlayout System):

- **Przechwytywanie nie szybsze niż klatkaż kanału.** Chromium składa
  stronę 60 razy na sekundę; przy 24–30 fps screencast bierze co drugą
  klatkę (30 PNG/s zamiast 60), przy 50–60 fps każdą. To połowa pracy
  z PNG w Chromium i sidecarze, więc pod obciążeniem klatki przychodzą
  równo. Pojedyncza zmiana (strona statyczna, ostatnia klatka animacji)
  mogłaby wypaść między branymi klatkami, dlatego po 150 ms bez klatek
  sidecar robi jeden zrzut strony (`Page.captureScreenshot`) i obraz zawsze
  dochodzi do stanu końcowego.
- **Każda klatka wyjścia pokazuje grafikę ze swojej chwili.** Sidecar trzyma
  klatki z ostatnich 0,5 s z czasem, w którym Chromium je narysował (znacznik
  screencastu, nie chwila końca dekodowania), a strumień bierze dla klatki
  wyjścia tę, która była aktualna w jej chwili, nie w chwili zapisu. Klatka
  wyjścia patrzy 100 ms wstecz: tyle mieści się przesył i dekodowanie PNG,
  więc każda zastaje już swój obraz, a rozrzut czasu dekodowania nie zamienia
  ruchu w powtórzenia i podwójne kroki (opóźnienie grafiki rośnie o 0,1 s).
  Gdy ErsatzTV na moment przestaje czytać (zator, obciążony CPU), zaległe
  klatki idą jako kolejne obrazy animacji, a nie jeden obraz powtórzony i skok.

Strumień wyprzedza zegar najwyżej o `lead_seconds`
(domyślnie 4 s) – to bufor, który ErsatzTV buduje początkowym wyrzutem,
i zarazem opóźnienie zmiany z VizRS na obrazie. Mniej = szybsza grafika,
więcej = gdy odtwarzanie się zacina.

Gdy przez 30 s nie ma żadnej sesji danego rozmiaru (ErsatzTV transkoduje
kanał tylko przy widzach), przechwytywanie przechodzi w `paused`: Chromium
zostaje uruchomiony ze stroną `/output`, ale nie renderuje klatek (przy
animowanym pasku 1080p z ~200% CPU do kilku procent). Pierwsza sesja
wznawia je i od razu dostaje świeżą klatkę. Przerwa między pozycjami
harmonogramu jest dużo krótsza, więc w trakcie emisji pauza nie występuje.

## 2. Odpowiedniki w VizRS ffPlayout System

VizRS ffPlayout System jest od 2026-10-24 archiwum: nie jest rozwijany i
zostaje jako wzorzec. Tabela pokazuje, skąd pochodzą opcje i jak przenieść
szablony komend; nowe funkcje VirtRS CG nie mają już odpowiednika tam.

| ffPlayout | ErsatzTV / VirtRS CG |
|---|---|
| Ustawienia → grafika VizRS: adres, token | `vizrs.address`, `vizrs.token` |
| komendy klipu / reklamy | `on_clip` / `on_ad` |
| URL wtapiania | `output_url` |
| reklama = `"category": "advertisement"` | `ad_match.path` / `ad_match.title` (wyrażenia regularne na ścieżce i tytule) |
| wtapianie w łańcuchu filtrów ffmpeg | element grafiki „script” ErsatzTV, `z_index: 100` |
| reklama zaznaczona na pozycji | pozycja harmonogramu bez elementu **VizRS** (`ad_without_element`) |
| lista `vizrs` na pozycji playlisty | brak (ErsatzTV nie ma pól na pozycję) |
| `{next_*}` z playlisty | z wpisów EPG ErsatzTV (`epg_entries: 3`) |
| przechwytywanie co n-tą klatkę Chromium, wtapianie aktualnej grafiki w każdą klatkę | to samo (§1, płynność animacji), plus zrzut stanu końcowego |
| brak – grafika zawsze w rozmiarze kanału | `render_size` / `&render=` (rozmiar renderowania grafiki, §1) |

## 3. Komendy i symbole zastępcze

Jedna komenda na linię (składnia: [API VizRS, TCP](../../VizRS%20CG%20System/docs/API.md));
puste linie i komentarze `#` są pomijane. Przy tokenie sidecar najpierw
wysyła `AUTH <token>`.

| Symbol | Wartość |
|---|---|
| `{title}` | tytuł, tytuł serialu albo nazwa pliku |
| `{show_title}` | tytuł serialu |
| `{source}` | ścieżka pliku |
| `{duration}` | długość klipu `hh:mm:ss` |
| `{next_title}` | tytuł następnego programu |
| `{next_start}` / `{next_start_full}` | start następnego, `hh:mm` / `hh:mm:ss` |
| `{next_duration}` | długość następnego `hh:mm:ss` |

`|` w wartościach zamienia się na `/`, a znaki końca linii są usuwane.
Gdy kilku widzów dołącza do tej samej pozycji, ErsatzTV uruchamia skrypt
ponownie – komendy nie są wysyłane drugi raz.

Na reklamie sidecar odczytuje `STATE`, zdejmuje wszystkie warstwy na antenie
i wysyła `on_ad`. Pierwszy zwykły klip przywraca zdjęte strony jednym `TAKE`
przed własnymi komendami `on_clip`.

Reklamę można też wskazać w harmonogramie, jak znak wodny: element **VizRS**
dodaj w **Schedules → pozycja → Graphics Elements** przy pozycjach z grafiką,
a przy reklamach go nie zaznaczaj (wtedy nie dodawaj go do **Deco**).
ErsatzTV nie uruchamia wtedy skryptu, więc sidecar nie dostaje sesji. Gdy
żadna sesja kanału nie trwa w chwili końca poprzedniej pozycji
(`MediaItem_Stop`) plus 2 s, sidecar traktuje to jako reklamę: zdejmuje
warstwy na antenie (`STATE` → `OUT`) i wysyła `on_ad` (symbole klipu są wtedy
puste). Pierwsza pozycja z elementem przywraca strony. Wyłącza to
`ad_without_element: false`. Uwaga: zatrzymanie kanału w ErsatzTV (brak
widzów) wygląda tak samo i też zdejmuje grafikę – wraca przy kolejnej sesji.

## 4. Uruchamianie w Dockerze

```sh
docker compose up -d
```

uruchamia VizRS (`:8788`), sidecar (`:7700`, `/health`) i ErsatzTV (`:8409`,
media z `./media` albo `MEDIA_DIR`). Compose montuje
[vizrs.yml](../etv/templates/graphics-elements/script/vizrs.yml) do
`/config/templates/graphics-elements/script/`, a klienta do `/vizrs`.
W ErsatzTV dodaj element **VizRS** do pozycji harmonogramu
(**Schedules → pozycja → Graphics Elements**) albo do **Deco** przypisanego
do playoutu i zresetuj playout – bez tego ErsatzTV nie uruchamia skryptu.
Przy kilku kanałach skopiuj element z `channel=2`, … w URL.

Przed otwarciem portów zmień `VIZRS_TOKEN` i `VIZRS_OUTPUT_TOKEN`.
Ustawienia stałe dla danej maszyny (nazwa projektu, nakładki, porty, tokeny)
trzymaj w pliku `.env` obok compose (poza gitem), np.
`COMPOSE_PROJECT_NAME=virtrs`, `COMPOSE_FILE=docker-compose.yml:docker-compose.nvidia.yml`,
`VIZRS_PORT=8789` – wtedy samo `docker compose pull && docker compose up -d --build`
odtwarza ten sam stos.
Chromium wymaga `shm_size: 512mb`. Obraz buduje się na amd64 i arm64.

### Wersje ErsatzTV i Node.js

Zasady:

- **ErsatzTV** – zawsze najnowsza wersja z
  [github.com/ErsatzTV/legacy](https://github.com/ErsatzTV/legacy.git);
  compose używa obrazu `ghcr.io/ersatztv/legacy:latest` (amd64 i arm64).
- **Node.js** – zawsze najnowsza wersja z
  [github.com/nodejs/node](https://github.com/nodejs/node.git).

Integrację, testy i obrazy sprawdzamy na tych wersjach; zgłoszenia na
starszych najpierw powtarzamy po aktualizacji.

### Oracle Cloud Ampere A1

Na maszynie Ampere A1 (arm64, bez GPU) z nakładką:

```sh
docker compose -f docker-compose.yml -f docker-compose.ampere.yml pull
docker compose -f docker-compose.yml -f docker-compose.ampere.yml up -d
```

Obraz sidecara `ghcr.io/sdorpl/virtrs-cg` (amd64 i arm64, zmienna
`CG_IMAGE`) buduje CI po znaczniku `v*` (`.github/workflows/ci.yml`); pakiet
w ghcr musi być publiczny albo maszyna zalogowana (`docker login ghcr.io`).
Bez opublikowanego obrazu dodaj `--build` do `up` – zbuduje go sama A1
(kilka minut; budowanie krzyżowe przez QEMU na x86 jest dużo wolniejsze).
Obraz VizRS ma wariant arm64.

Nakładka ogranicza sidecar do 1,5 CPU i 2 GB (`CG_CPUS`, `CG_MEMORY`), uruchamia
Chromium z dwoma wątkami rastra (`VIZRS_BROWSER_ARGS`), dopasowuje libvips
i pulę wątków Node, daje ErsatzTV wyższy priorytet CPU, trzyma `/transcode`
w tmpfs 4 GB (`TRANSCODE_TMPFS`) i podnosi `LEAD_SECONDS` do 5. Brak
sprzętowego kodera: w profilu FFmpeg ErsatzTV ustaw **Hardware Acceleration:
None**, libx264, preset `veryfast` (`superfast` przy kilku kanałach albo
1080p50). Na darmowym kształcie 4 OCPU realnie jeden kanał 1080p albo dwa
720p transkodowane jednocześnie. Otwórz porty w liście bezpieczeństwa VCN
i w zaporze maszyny (`firewalld`/`iptables`).

Grafika na A1 kosztuje głównie przy animacji: przesuwający się pasek 1080p
to ok. 2 rdzenie Chromium (pomiar na x86), więc przy limicie `CG_CPUS`
animacje gubią klatki, a obraz statyczny (logo, plansza, zegar co sekundę)
prawie nic nie kosztuje. Bez widzów przechwytywanie stoi (§1). Lżejsze
`/output` (mniej ciągłych animacji, mniejsze rozmiary kanału) daje więcej
niż podnoszenie `CG_CPUS`.

Limity darmowego planu (Always Free): 4 OCPU i 24 GB łącznie na wszystkie
instancje A1, 200 GB dysków, 10 TB ruchu wychodzącego miesięcznie – średnio
ok. 31 Mb/s, czyli ok. 7 widzów HLS 4 Mb/s oglądających non stop. Oracle
odzyskuje darmowe instancje bezczynne przez 7 dni (jednocześnie CPU
w 95. percentylu, sieć i pamięć poniżej 20%); kanał na żądanie bez widzów
tak wygląda. Konto Pay As You Go z tymi samymi
darmowymi limitami temu nie podlega.

### NVIDIA NVENC

Koduje ErsatzTV; jego ffmpeg ma `h264_nvenc`, `hevc_nvenc` i filtry CUDA.
Ze sterownikiem NVIDIA i
[NVIDIA Container Toolkit](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/install-guide.html):

```sh
docker compose -f docker-compose.yml -f docker-compose.nvidia.yml up -d
```

potem **Settings → FFmpeg Profiles → Hardware Acceleration** = **Nvidia**.
Karta trafia do kontenera przez CDI (`driver: cdi`, `nvidia.com/gpu=…`), więc
Docker nie potrzebuje runtime'u `nvidia` w `daemon.json` (Docker 25+). Specyfikację
`/var/run/cdi/nvidia.yaml` toolkit (1.18+) generuje sam; bez niej:
`sudo nvidia-ctk cdi generate --output=/var/run/cdi/nvidia.yaml`. Karty pokazuje
`nvidia-ctk cdi list`, a `NVIDIA_GPU` wybiera jedną (indeks albo UUID, domyślnie `all`).
Sprawdzenie: `docker compose exec ersatztv nvidia-smi` i
`docker compose exec ersatztv ffmpeg -hide_banner -f lavfi -i testsrc2 -t 3 -c:v h264_nvenc -f null -`.
Komunikat `could not select device driver "nvidia"` oznacza starą wersję nakładki
albo brak specyfikacji CDI. Grafika się nie
zmienia – trafia do ErsatzTV jako surowe klatki RGBA, więc sidecar i jego
Chromium nie potrzebują GPU. Karty konsumenckie ograniczają liczbę
równoległych sesji NVENC, czyli kanałów transkodowanych naraz.

## 5. Uruchamianie bez Dockera, konfiguracja

Node 22+ i Chromium (`chromium-headless-shell`, `chromium`, `google-chrome`,
… w `PATH` – w tej kolejności – albo `VIZRS_BROWSER`). Obraz Dockera używa
`chromium-headless-shell` (Debian): ten sam silnik bez warstwy interfejsu,
przy animacji ok. połowa CPU pełnego `chromium`.

```sh
node bin/virtrs-cg.js config.json
```

```json
{
  "listen": "0.0.0.0:7700",
  "vizrs": { "address": "127.0.0.1:6100", "token": "" },
  "output_url": "http://127.0.0.1:8088/output",
  "on_clip": "TAKE 4001 headline={title}|sub=Potem: {next_title} o {next_start}",
  "on_ad": "",
  "ad_match": { "path": "/reklamy/", "title": "" },
  "ad_without_element": true,
  "lead_seconds": 4,
  "render_size": ""
}
```

Pusty `output_url` = tylko komendy, bez wtapiania. Wzór: `config.example.json`.

Zmienne środowiskowe (nadpisują plik): `LISTEN`, `VIZRS_ADDRESS`,
`VIZRS_TOKEN`, `VIZRS_OUTPUT_URL`, `VIZRS_ON_CLIP`, `VIZRS_ON_AD`
(`\n` rozdziela linie), `VIZRS_AD_PATH`, `VIZRS_AD_TITLE`, `VIZRS_AD_WITHOUT_ELEMENT=0`
(wyłącza reklamę po braku elementu), `LEAD_SECONDS`, `VIZRS_RENDER_SIZE`
(`720p`, `1280x720`, puste = rozmiar kanału; §1),
`VIZRS_BROWSER`, `VIZRS_BROWSER_NO_SANDBOX=1` (root / kontenery),
`VIZRS_BROWSER_ARGS` (dodatkowe flagi Chromium, rozdzielone spacją),
`DEBUG=1`.

## 6. Awarie

Grafika nigdy nie zatrzymuje playoutu. Komendy idą w tle z limitem 2 s na
połączenie i każdą odpowiedź; błędy i odpowiedzi `ERR` trafiają do logu.
Nieudane przywrócenie stron po reklamie jest ponawiane na kolejnym klipie.
Po awarii przeglądarki grafika jest czyszczona, a Chromium wraca po 3 s.
Przeglądarka ma 30 s na start i otwarcie `/output` (pierwszy start na zimnej
maszynie, np. małej VM ARM, potrafi trwać 15–20 s); potem jest restartowana.
Gdy sidecar jest nieosiągalny, klient kończy pracę i pozycja leci bez grafiki.

## 7. Testy

```sh
npm test
```

Testy jednostkowe, test integracyjny z udawanym VizRS i klientem ErsatzTV
oraz prawdziwe przechwytywanie Chromium (pomijane bez przeglądarki).

### Sprzątanie po testach w Dockerze

Po testach na stosie Dockera zawsze posprzątaj: usuń kontenery, obrazy,
wolumeny, sieci i pliki (zrzuty PNG, logi), które powstały tylko na
potrzeby testu. **Nie ruszaj danych działających instancji tego projektu**
– wolumenów podpiętych do jego obecnie działających kontenerów – żeby nie
trzeba było ich bez powodu kopiować i konfigurować od nowa.

Przed usunięciem wolumenu sprawdź, czy nie używa go działający kontener:

```sh
docker ps --filter volume=<wolumen>    # pusto = wolumen niczyj, można usunąć
```

Nie używaj `docker compose down -v`, `docker volume prune` ani `docker
system prune --volumes` – usuwają dane bez takiego sprawdzenia.

CI (`.github/workflows/ci.yml`) uruchamia testy przy każdym wypchnięciu na
`main` i w PR, a po znaczniku `v*` (albo ręcznie) buduje obraz sidecara
na amd64 i arm64 i publikuje go jako `ghcr.io/<właściciel>/virtrs-cg`.

## 8. Benchmark silników: Chromium a WPE WebKit

`bench/` porównuje na tej samej maszynie dwa silniki renderujące grafikę
bez GPU:

- **Chromium** (`chromium-headless-shell`) przechwytywany dokładnie jak
  w sidecarze – klasa `Capture` z `src/capture.js` (screencast PNG co n-tą
  klatkę, dekodowanie przez `sharp`, przycinanie),
- **WPE WebKit** – silnik WebKit dla urządzeń wbudowanych, przez element
  GStreamera `wpevideosrc` (surowe klatki RGBA z przezroczystością, bez PNG),
  z tym samym przycinaniem.

Oba renderują stronę `bench/scene.html`, podobną do grafiki VizRS:
przezroczyste tło, zegar, logo obracane w 3D (`perspective`), lower third
z gradientem, `drop-shadow` i `blur`, wysuwany co 5 s, tekst przez
`EventSource`, ticker na `requestAnimationFrame` ograniczony do klatkażu.
Scena `static` ma tylko zegar i zmieniany tekst, `animated` – wszystko.
Zamiast sceny można zmierzyć prawdziwe `/output` VizRS (`BENCH_URL`, scena
`url`; grafika musi być na antenie).

Każdy przebieg (silnik × scena) to rozgrzewka i pomiar. Raport pod
`http://<serwer>:7790/` pokazuje:

| Kolumna | Znaczenie |
|---|---|
| nowe klatki/s | zmienione obrazy z widoczną grafiką (animacja przy 25 fps: ok. 25) |
| przerwa p95 / max | odstępy między nowymi klatkami – równomierność animacji |
| CPU silnik | procesy przeglądarki albo GStreamera, % jednego rdzenia |
| CPU node | dekodowanie PNG (Chromium) albo odbiór surowych klatek (WPE) i przycinanie – praca sidecara |
| PSS silnika | pamięć procesów silnika (część współdzielona liczona proporcjonalnie) |

oraz po jednej klatce z każdego silnika na szachownicy, żeby porównać
wierność obrazu (czcionki, cienie, rozmycie, przezroczystość).

Zmienne środowiskowe (wszystkie opcjonalne):

| Zmienna | Domyślnie | Znaczenie |
|---|---|---|
| `BENCH_ENGINES` | `chromium,wpe` | silniki |
| `BENCH_SCENES` | `static,animated` (+`url` przy `BENCH_URL`) | sceny |
| `BENCH_URL` | – | adres do zmierzenia, np. `http://vizrs:8088/output?token=…&fps=25` |
| `BENCH_SIZE` | `1920x1080` | rozmiar renderowania |
| `BENCH_FPS` | `25` | klatkaż kanału |
| `BENCH_WARMUP` | `10` | rozgrzewka przed pomiarem [s] |
| `BENCH_DURATION` | `60` | pomiar [s] |
| `BENCH_PORT` | `7790` | port raportu na hoście |

Z domyślnymi wartościami całość trwa ok. 5 min. Mierz na maszynie
docelowej i bez innego obciążenia (zatrzymaj na ten czas sidecar, jeśli
działa na tym samym hoście) – wyniki z różnych maszyn nie są porównywalne.

### Instalacja w Portainerze

Obraz buduje się z repozytorium na maszynie docelowej (amd64 albo arm64,
np. Ampere A1); pierwszy build trwa kilka minut.

1. **Stacks → Add stack**, nazwa np. `virtrs-bench`.
2. **Build method: Repository**:
   - Repository URL: `https://github.com/sdorpl/VirtRS-CG-Playout`
   - Repository reference: `refs/heads/main`
   - Compose path: `bench/docker-compose.yml`
   - (repozytorium jest publiczne – bez uwierzytelniania)
3. **Environment variables** (opcjonalnie): np. `BENCH_DURATION=120`, a do
   pomiaru prawdziwej grafiki `BENCH_URL` z adresem `/output` osiągalnym
   z kontenera (adres hosta, nie `localhost`).
4. **Deploy the stack**. Postęp: **Containers → virtrs-bench-bench-1 → Logs**.
5. Raport: `http://<serwer>:7790/` – odświeża się sam, dopóki pomiar trwa.
   Wyniki zostają też w wolumenie `bench-results` (`results.json`, klatki PNG).
6. Kolejny pomiar: **Containers → virtrs-bench-bench-1 → Restart**. Po zmianie
   zmiennych: **Stacks → virtrs-bench → Editor/Environment → Update the stack**.
7. Po zakończeniu sprzątanie (§7): **Stacks → virtrs-bench → Delete**, potem
   w **Volumes** usuń `virtrs-bench_bench-results`, a w **Images**
   `virtrs-cg-bench:local`.

Bez Portainera, z katalogu repozytorium:

```sh
docker compose -f bench/docker-compose.yml -p virtrs-bench up --build -d
docker compose -f bench/docker-compose.yml -p virtrs-bench logs -f
# sprzątanie
docker compose -f bench/docker-compose.yml -p virtrs-bench down --rmi local
docker volume rm virtrs-bench_bench-results
```

### Uwagi

- WebKit, podobnie jak Chromium, nie uruchomi w kontenerze własnej
  piaskownicy; benchmark ją wyłącza
  (`WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1`) – to odpowiednik
  `--no-sandbox`.
- WPE oddaje surowe klatki w stałym tempie (8 MB na klatkę przy 1080p); ten
  sam obraz jest odrzucany porównaniem bufora, zanim trafi do przycinania.
- Pierwsze krótkie pomiary (Xeon E3-1270 v3, amd64, 1080p, 25 fps, scena
  `animated`) dały WPE więcej CPU (260–310% wobec 205–230% dla Chromium)
  i więcej pamięci (375–390 MB wobec 230–260 MB PSS) przy tej samej
  liczbie klatek, a `FontFace` z `local()` w WPE się nie załadował.
  Rozstrzyga pomiar na Ampere A1.
