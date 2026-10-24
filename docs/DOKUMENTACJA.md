# VirtRS CG Playout – dokumentacja

1. [Jak to działa](#1-jak-to-działa)
2. [Odpowiedniki w VizRS ffPlayout System](#2-odpowiedniki-w-vizrs-ffplayout-system)
3. [Komendy i symbole zastępcze](#3-komendy-i-symbole-zastępcze)
4. [Uruchamianie w Dockerze](#4-uruchamianie-w-dockerze)
5. [Uruchamianie bez Dockera, konfiguracja](#5-uruchamianie-bez-dockera-konfiguracja)
6. [Awarie](#6-awarie)
7. [Testy](#7-testy)
8. [Benchmark przechwytywania grafiki](#8-benchmark-przechwytywania-grafiki)

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
| `GET /ffmpeg?channel=1[&wait=2000]` | nakładka ffmpeg przejmuje wolną sesję elementu kanału; odpowiedź: `session`, `width`, `height`, `fps` (404 = brak sesji) |
| `GET /ffmpeg/frames?session=N[&fps=25/1]` | surowe klatki `yuva420p` przejętej sesji dla ffmpeg (Nakładka ffmpeg, niżej) |
| `GET /health` | liczba sesji, stan przechwytywania (`size` kanału, `render`, `status`: `capturing`, `paused`, `restarting`…, `seq`) i kanały z nakładką ffmpeg (`overlay`) |

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

### Nakładka ffmpeg

Element „script” ma w ErsatzTV stały koszt, niezależny od grafiki: dla
każdej klatki silnik grafiki ErsatzTV (.NET) rysuje płótno w rozmiarze kanału,
koduje je do FFV1 w osobnym ffmpeg, a główny ffmpeg je dekoduje
i konwertuje. Pomiar na Xeon E3-1270 v3 z NVENC, kanał 1080p25: przez ten
łańcuch przechodzi ok. 14 kl./s, transkodowanie idzie 0,5–0,57× czasu
rzeczywistego (segment 4 s co ok. 7 s), a widz stale czeka na bufor. Ani
sidecar (równe 25 kl./s), ani główny ffmpeg (ok. 5× z grafiką z pliku) tego
nie ograniczają, a `render_size` nie pomaga, bo płótno ErsatzTV zawsze ma
rozmiar kanału.

Nakładka [vizrs-ffmpeg.py](../client/vizrs-ffmpeg.py) omija ten łańcuch, tak
jak VizRS ffPlayout System mieszał grafikę we własnym ffmpeg. W ErsatzTV
ustawia się ją jako ścieżkę FFmpeg (**Settings → FFmpeg → FFmpeg Path** =
`/vizrs/vizrs-ffmpeg.py`); kod ErsatzTV zostaje bez zmian.

```
ErsatzTV ── ffmpeg ──► vizrs-ffmpeg.py ── -i pipe:N (yuva420p) ◄── GET /ffmpeg/frames ── sidecar
              │            └─ GET /internal/graphics/… (płótno: start elementu „script”, dane pozycji)
              └─ każde inne wywołanie: prawdziwy ffmpeg bez zmian
```

1. Wywołanie bez wejścia płótna grafiki (`-i http://…/internal/graphics/<kanał>/<pozycja>`)
   idzie do prawdziwego ffmpeg bez zmian (`exec`). Płótno z parametrem
   `region` (same watermarki, bez elementu VizRS) też.
2. Nakładka sama otwiera płótno z nagłówkami ErsatzTV i odczytuje je do
   końca. ErsatzTV uruchamia więc element „script” jak zwykle: klient
   przekazuje dane pozycji, a komendy, reklamy i `{next_*}` działają bez zmian.
3. `GET /ffmpeg?channel=<kanał>` przejmuje sesję tego elementu (czeka na nią
   do 2 s). Sidecar przestaje wysyłać do niej pakiety, a silnik grafiki
   ErsatzTV czeka na następny pakiet, nie zużywając CPU.
4. ffmpeg czyta wejście grafiki z gniazda `GET /ffmpeg/frames` przekazanego
   jako `pipe:N`. Są to surowe klatki `yuva420p` w rozmiarze kanału,
   w tempie kanału z `lead_seconds` zapasu (§1). Opcje płótna (`-headers`,
   `-protocol_whitelist`, kodek, format) znikają, a `-t` i reszta komendy
   zostają, łącznie z filtrem nakładania ErsatzTV (`overlay_cuda` itd.).
5. Gdy ffmpeg kończy kawałek, sidecar kończy sesję elementu, a nakładka po
   0,5 s zamyka płótno. ErsatzTV kończy wtedy element „script” jak bez
   nakładki.

Kanał, którego sesję przejęła nakładka, sidecar zapamiętuje. Kolejne sesje
tego kanału od razu nic nie wysyłają do ErsatzTV. Jeśli nakładka nie
przejmie sesji w 3 s, sidecar zapomina kanał i pakiety idą do ErsatzTV jak
dawniej.

Ten sam pomiar z nakładką: transkodowanie 1× (na początku kawałka szybciej,
o zapas `lead_seconds`), CPU kontenera ErsatzTV ok. 20–30% zamiast ok. 360%,
sidecar ok. 15%.

Warunki i różnice:

- Numer w `channel=` adresu elementu musi być numerem kanału ErsatzTV.
  Nakładka zna kanał tylko z adresu płótna.
- Z nakładką ErsatzTV nie rysuje własnych elementów grafiki tej pozycji
  (tekst, obraz, watermarki z płótna). Logo i napisy daj do VizRS.
- Kolory: klatki są w BT.709 (od 720 linii) albo BT.601, w zakresie
  ograniczonym, z alfą prostą (nie premultiplikowaną). Ścieżka ErsatzTV
  konwertowała zawsze w BT.601, więc w HD kolory grafiki są teraz wierniejsze.
- Pozycja z innymi elementami grafiki, ale bez elementu VizRS, startuje
  o 2 s później (nakładka czeka na sesję) i leci ścieżką ErsatzTV.
- Na wejściu surowe klatki kosztują pasmo, ale nie CPU: 1080p to ok. 5 MB
  na klatkę, ok. 130 MB/s przy 25 kl./s. Idą lokalnie przez sieć Dockera.

Zmienne środowiskowe nakładki (w kontenerze ErsatzTV):

| Zmienna | Domyślnie | Znaczenie |
|---|---|---|
| `VIZRS_CG_URL` | – (compose: `http://cg:7700`) | adres sidecara; puste = nakładka tylko przekazuje wywołania do ffmpeg |
| `VIZRS_FFMPEG` | `/usr/local/bin/ffmpeg` | prawdziwy ffmpeg |
| `VIZRS_FFMPEG_FPS` | – | wymuszony klatkaż kanału, np. `50`; puste = klatkaż materiałów |

#### Klatkaż kanału (`VIZRS_FFMPEG_FPS`)

ErsatzTV nie ma ustawienia klatkażu kanału. Bierze klatkaż materiałów, a przy
**Normalize Framerate** w profilu FFmpeg najniższy z nich. Kanał z materiałami
25 kl./s ma więc 25 kl./s. Z `VIZRS_FFMPEG_FPS=50` nakładka zmienia każde
transkodowanie do HLS:

- do wideo przed pierwszym filtrem nakładania wstawia `fps=50`
  (`[v_m0][v_s0]overlay…` → `[v_m0]fps=50[v_m0_fps];[v_m0_fps][v_s0]overlay…`),
  więc grafika jest nakładana i porusza się w 50 kl./s, a nie tylko dublowana
  na wyjściu;
- przed `-f hls` dodaje `-r 50`, także w pozycjach bez grafiki VizRS (reklamy,
  komenda oryginalna), żeby cały kanał miał jeden klatkaż;
- surowe klatki grafiki idą w 50 kl./s, a sidecar przestawia przechwytywanie
  Chromium na każdą klatkę (`/health`: `fps` przechwytywania).

Inne wywołania (koder płótna ErsatzTV, kopiowanie strumienia `-c copy`,
remuks do MPEG-TS) zostają bez zmian. Obraz materiału 25 kl./s nie robi się
płynniejszy, bo każda jego klatka jest powtórzona; płynniejsza jest grafika.
Koszt: animacja grafiki to dwa razy więcej PNG w Chromium i sidecarze (pomiar
na Xeon E3-1270 v3, 1080p50: ok. 170% CPU sidecara w trakcie animacji, kilka
procent przy grafice stojącej), surowe klatki 1080p to ok. 260 MB/s, a koder
dostaje dwa razy więcej klatek (`-g 50` z profilu oznacza wtedy GOP 1 s).

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
| wtapianie w łańcuchu filtrów ffmpeg | nakładka ffmpeg (§1), bez niej element grafiki „script” ErsatzTV, `z_index: 100` |
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
media z `./seed/media` albo `MEDIA_DIR`). Compose montuje
[vizrs.yml](../etv/templates/graphics-elements/script/vizrs.yml) do
`/config/templates/graphics-elements/script/`, a klienta do `/vizrs`.
W ErsatzTV dodaj element **VizRS** do pozycji harmonogramu
(**Schedules → pozycja → Graphics Elements**) albo do **Deco** przypisanego
do playoutu i zresetuj playout – bez tego ErsatzTV nie uruchamia skryptu.
Przy kilku kanałach skopiuj element z `channel=2`, … w URL.

Compose montuje też [nakładkę ffmpeg](../client/vizrs-ffmpeg.py) (§1) do
`/vizrs` i ustawia ErsatzTV `VIZRS_CG_URL=http://cg:7700`. Baza startowa
(`seed/`) ma już ścieżkę FFmpeg `/vizrs/vizrs-ffmpeg.py`. W istniejącej
instancji ustaw ją w **Settings → FFmpeg → FFmpeg Path**. Powrót do ścieżki
grafiki ErsatzTV: `/usr/local/bin/ffmpeg` albo `VIZRS_CG_URL=` (puste).

Przed otwarciem portów zmień `VIZRS_TOKEN` i `VIZRS_OUTPUT_TOKEN`.
Ustawienia stałe dla danej maszyny (nazwa projektu, nakładki, porty, tokeny)
trzymaj w pliku `.env` obok compose (poza gitem), np.
`COMPOSE_PROJECT_NAME=virtrs`, `COMPOSE_FILE=docker-compose.yml:docker-compose.nvidia.yml`,
`VIZRS_PORT=8789` – wtedy samo `docker compose pull && docker compose up -d --build`
odtwarza ten sam stos.
Chromium wymaga `shm_size: 512mb`. Obraz buduje się na amd64 i arm64.

### Dane startowe (`seed/`)

Pierwszy start stosu (także w Portainerze z repozytorium) dostaje gotową
konfigurację instancji wzorcowej: usługa `seed` kopiuje `seed/` do **pustych**
wolumenów i kończy pracę, a VizRS i ErsatzTV startują po niej. Wolumen
z danymi zostaje nietknięty (w logu `seed: … present, left alone`), więc
`docker compose up -d` na działającej instancji niczego nie nadpisuje.

| Katalog | Zawartość | Trafia do |
|---|---|---|
| `seed/vizrs-data/` | show „Newsroom INFO24” (`shows/newsroom-info24.json`, aktywny), `show.json`, tła z `assets/` | wolumen `vizrs-data` (`/data` VizRS) |
| `seed/ersatztv-config/ersatztv.sqlite3` | baza ErsatzTV: kanał 1 z elementem **VizRS**, harmonogram, biblioteka `/media`, profil FFmpeg 1080p x264, ścieżka FFmpeg = nakładka (§1) | wolumen `ersatztv-config` (`/config`) |
| `seed/media/` | odcinki testowe i reklama (`reklamy/` – bez grafiki) | montowany jako `/media` (domyślne `MEDIA_DIR`) |

Bez sekretów: nie ma tu plików `*-secrets.json`, kluczy `data-protection`,
logów, cache ani indeksu wyszukiwania (ErsatzTV buduje go przy pierwszym
starcie – ostrzeżenie `Search index failed to initialize` jest wtedy
normalne). Profil FFmpeg koduje programowo (Hardware Acceleration: None),
żeby kanał ruszył na każdej maszynie; z kartą NVIDIA przełącz go na Nvidia
(niżej). Tokeny VizRS ustawiasz jak zwykle w `.env`.

Odświeżenie danych startowych z działającej instancji: zatrzymaj ErsatzTV
(`docker compose stop ersatztv`, baza bez otwartego WAL), skopiuj z wolumenów
`show.json`, `shows/`, `assets/` i `ersatztv.sqlite3` do `seed/`, w kopii
bazy ustaw `FFmpegProfile.HardwareAcceleration = 0` i `ffmpeg.ffmpeg_path =
/vizrs/vizrs-ffmpeg.py` w `ConfigElement`, usuń wpis
`search_index.version` z `ConfigElement`, potem `docker compose start ersatztv`.
Własne media trzymaj w `media/` (poza gitem) i wskaż je przez
`MEDIA_DIR=./media`; ścieżki w ErsatzTV zaczynają się od `/media`.

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
zmienia – trafia do ffmpeg ErsatzTV jako surowe klatki (§1), więc sidecar i jego
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

Nakładka ffmpeg uruchamia oryginalną komendę ErsatzTV (z płótnem grafiki
ErsatzTV), gdy sidecar nie odpowiada, nie ma sesji elementu VizRS albo
strumień klatek się nie otwiera. Do logu ErsatzTV trafia wtedy linia
`vizrs-ffmpeg: …; ErsatzTV graphics instead`. Przerwany strumień klatek
w trakcie kawałka zostawia ostatni obraz grafiki do końca kawałka (najwyżej
kilkadziesiąt sekund), bo ffmpeg ErsatzTV przy końcu wejścia nakładki
powtarza jego ostatnią klatkę; obraz programu leci dalej. Gdy ErsatzTV zabija
nakładkę, ffmpeg dostaje `SIGTERM` (`PR_SET_PDEATHSIG`); sygnały zamknięcia
(`SIGTERM`, `SIGINT`, …) nakładka przekazuje ffmpeg.

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

## 8. Benchmark przechwytywania grafiki

`bench/` mierzy na maszynie docelowej, ile kosztuje i jak płynna jest
grafika renderowana bez GPU. Warianty (`BENCH_ENGINES`):

- **`chromium`** – dokładnie to, co sidecar: klasa `Capture` z
  `src/capture.js` (screencast PNG co n-tą klatkę, dekodowanie przez
  `sharp`, przycinanie) i strumień pakietów z `src/stream.js`,
- **`chromium-frames`** – prototyp rysowania zsynchronizowanego z klatkami
  wyjścia (`bench/frames-worker.mjs`): Chromium rysuje tylko na żądanie
  (`HeadlessExperimental.beginFrame`), raz na slot siatki 25 kl./s, z czasem
  slotu jako czasem klatki, więc animacje idą dokładnie o jedną klatkę na
  slot; bez zmiany na stronie nie ma PNG. Kodowanie PNG w Chromium blokuje
  następną klatkę, dlatego `BENCH_FRAME_PAGES` (domyślnie 2) przeglądarek
  rysuje sloty na zmianę (karty jednej przeglądarki nie mogą – jej
  kompozytor pada na nakładających się klatkach). DevTools, dekodowanie
  i przycinanie działają w wątku roboczym (`worker_threads`),
- **`wpe`** – WPE WebKit przez element GStreamera `wpevideosrc` (surowe
  klatki RGBA, bez PNG); odpada (wyniki niżej), zostaje do porównań.

Wszystkie renderują stronę `bench/scene.html`, podobną do grafiki VizRS:
przezroczyste tło, zegar, logo obracane w 3D (`perspective`), lower third
z gradientem, `drop-shadow` i `blur`, wysuwany co 5 s, tekst przez
`EventSource`, ticker na `requestAnimationFrame` ograniczony do klatkażu.
Scena `static` ma tylko zegar i zmieniany tekst, `animated` – wszystko oraz
znacznik 4×4 px w lewym górnym rogu, którego kolor koduje krok animacji
(czas `requestAnimationFrame` / długość klatki). Zamiast sceny można
zmierzyć prawdziwe `/output` VizRS (`BENCH_URL`, scena `url`; grafika musi
być na antenie).

Każdy przebieg (rozmiar × scena × wariant) to rozgrzewka i pomiar. Raport
pod `http://<serwer>:7790/` pokazuje:

| Kolumna | Znaczenie |
|---|---|
| nowe klatki/s | zmienione obrazy z widoczną grafiką (animacja przy 25 fps: ok. 25) |
| ruch na wyjściu | scena `animated`: odsetek klatek wyjścia 25 kl./s, w których animacja poszła dokładnie o jeden krok (ideał 100%), stała albo przeskoczyła – to widać na antenie jako płynność |
| przerwa p95 / max | odstępy między nowymi klatkami |
| CPU silnik | procesy przeglądarki albo GStreamera, % jednego rdzenia |
| CPU node | dekodowanie PNG albo odbiór surowych klatek i przycinanie – praca sidecara |
| PSS silnika | pamięć procesów silnika (część współdzielona liczona proporcjonalnie) |

oraz po jednej klatce z każdego wariantu na szachownicy, żeby porównać
wierność obrazu (czcionki, cienie, rozmycie, przezroczystość).
W `results.json` wariant `chromium-frames` ma też liczniki wątku
(`frames_worker`: sloty, zrzuty, spóźnione sloty, czas `beginFrame`).

Zmienne środowiskowe (wszystkie opcjonalne):

| Zmienna | Domyślnie | Znaczenie |
|---|---|---|
| `BENCH_ENGINES` | `chromium,chromium-frames` | warianty (`chromium`, `chromium-frames`, `wpe`) |
| `BENCH_SCENES` | `static,animated` (+`url` przy `BENCH_URL`) | sceny |
| `BENCH_URL` | – | adres do zmierzenia, np. `http://vizrs:8088/output?token=…&fps=25` |
| `BENCH_SIZES` | `1920x1080,1280x720` | rozmiary renderowania, po przecinku (poza compose: `1920x1080`) |
| `BENCH_FRAME_PAGES` | `2` | przeglądarki `chromium-frames` rysujące sloty na zmianę |
| `BENCH_FPS` | `25` | klatkaż kanału |
| `BENCH_WARMUP` | `10` | rozgrzewka przed pomiarem [s] |
| `BENCH_DURATION` | `60` | pomiar [s] |
| `BENCH_PORT` | `7790` | port raportu na hoście |

Z domyślnymi wartościami całość trwa ok. 10 min. Mierz na maszynie
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

### Uwagi i wyniki

- **Ampere A1** (Neoverse-N1, 4 rdzenie, 1080p, 25 fps, 60 s): WPE przy
  animacji robi tylko 12,2 kl./s przy 243% CPU i 366 MB, Chromium 23,4
  kl./s przy 209% CPU i 223 MB; przy scenie statycznej oba ok. 36% CPU.
  WPE odpada.
- **`chromium-frames`** (Xeon E3-1270 v3 w Dockerze, Chromium 156, 12 s):
  ruch na wyjściu przy 720p 99% równych kroków wobec 65% w `chromium`, przy
  130% CPU wobec 149%; przy 1080p 71–92% wobec 65% przy podobnym CPU – tam
  ten procesor nie nadąża z PNG w każdym slocie. Scena statyczna kosztuje
  tyle co w `chromium`.
- Pułapki `beginFrame` w Chromium 156: z flagą
  `--disable-new-content-rendering-timeout` każda klatka zgłasza zmianę;
  żądanie zrzutu zawsze ją zgłasza, a klatka po zrzucie też – pochłania ją
  pusta klatka zaraz po zrzucie, a na stojącej stronie co sekundę idzie
  zrzut kontrolny.
- WebKit, podobnie jak Chromium, nie uruchomi w kontenerze własnej
  piaskownicy; benchmark ją wyłącza
  (`WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1`) – to odpowiednik
  `--no-sandbox`. WPE nie załadował `FontFace` z `local()`.
