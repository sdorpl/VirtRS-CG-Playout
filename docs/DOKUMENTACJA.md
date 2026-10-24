# VirtRS CG Playout – dokumentacja

1. [Jak to działa](#1-jak-to-działa)
2. [Odpowiedniki w VizRS ffPlayout System](#2-odpowiedniki-w-vizrs-ffplayout-system)
3. [Komendy i symbole zastępcze](#3-komendy-i-symbole-zastępcze)
4. [Uruchamianie w Dockerze](#4-uruchamianie-w-dockerze)
5. [Uruchamianie bez Dockera, konfiguracja](#5-uruchamianie-bez-dockera-konfiguracja)
6. [Awarie](#6-awarie)
7. [Testy](#7-testy)

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
strony, więc w sidecarze działa jedna przeglądarka na rozmiar kanału, także
między pozycjami.

Sidecar (Node 22):

| Adres | Co |
|---|---|
| `POST /etv?channel=1` | treść: dane szablonu ErsatzTV; odpowiedź: niekończący się strumień pakietów ETV |
| `GET /health` | liczba sesji i stan przechwytywania (`size`, `status`, `seq`) |

Chromium wysyła PNG tylko przy zmianie strony. Sidecar dekoduje najnowszą
klatkę, przycina ją do widocznych obszarów i wysyła jako jeden pakiet
`Rectangles` z osobnym prostokątem na każdą grupę grafik (np. logo w rogu
i pasek na dole to dwa małe prostokąty, a nie jeden na cały ekran;
przerwy węższe niż 32 px zostają w jednym prostokącie). Niezmienione klatki idą jako `Repeat`, więc statyczna grafika
prawie nic nie kosztuje. Animacje pod obciążeniem gubią klatki, ale nie
zostają w tyle. Strumień wyprzedza zegar najwyżej o `lead_seconds`
(domyślnie 4 s) – to bufor, który ErsatzTV buduje początkowym wyrzutem,
i zarazem opóźnienie zmiany z VizRS na obrazie. Mniej = szybsza grafika,
więcej = gdy odtwarzanie się zacina.

## 2. Odpowiedniki w VizRS ffPlayout System

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
Chromium wymaga `shm_size: 512mb`. Obraz buduje się na amd64 i arm64.

### Oracle Cloud Ampere A1

Na maszynie Ampere A1 (arm64, bez GPU) buduj na samej maszynie z nakładką:

```sh
docker compose -f docker-compose.yml -f docker-compose.ampere.yml up -d --build
```

Ogranicza sidecar do 1,5 CPU i 2 GB (`CG_CPUS`, `CG_MEMORY`), uruchamia
Chromium z dwoma wątkami rastra (`VIZRS_BROWSER_ARGS`), dopasowuje libvips
i pulę wątków Node, daje ErsatzTV wyższy priorytet CPU, trzyma `/transcode`
w tmpfs 4 GB (`TRANSCODE_TMPFS`) i podnosi `LEAD_SECONDS` do 5. Brak
sprzętowego kodera: w profilu FFmpeg ErsatzTV ustaw **Hardware Acceleration:
None**, libx264, preset `veryfast` (`superfast` przy kilku kanałach albo
1080p50). Na darmowym kształcie 4 OCPU realnie jeden kanał 1080p albo dwa
720p transkodowane jednocześnie. Otwórz porty w liście bezpieczeństwa VCN
i w zaporze maszyny (`firewalld`/`iptables`).

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

Node 22+ i Chromium (`chromium`, `google-chrome`, … w `PATH` albo
`VIZRS_BROWSER`):

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
  "lead_seconds": 4
}
```

Pusty `output_url` = tylko komendy, bez wtapiania. Wzór: `config.example.json`.

Zmienne środowiskowe (nadpisują plik): `LISTEN`, `VIZRS_ADDRESS`,
`VIZRS_TOKEN`, `VIZRS_OUTPUT_URL`, `VIZRS_ON_CLIP`, `VIZRS_ON_AD`
(`\n` rozdziela linie), `VIZRS_AD_PATH`, `VIZRS_AD_TITLE`, `VIZRS_AD_WITHOUT_ELEMENT=0`
(wyłącza reklamę po braku elementu), `LEAD_SECONDS`,
`VIZRS_BROWSER`, `VIZRS_BROWSER_NO_SANDBOX=1` (root / kontenery),
`VIZRS_BROWSER_ARGS` (dodatkowe flagi Chromium, rozdzielone spacją),
`DEBUG=1`.

## 6. Awarie

Grafika nigdy nie zatrzymuje playoutu. Komendy idą w tle z limitem 2 s na
połączenie i każdą odpowiedź; błędy i odpowiedzi `ERR` trafiają do logu.
Nieudane przywrócenie stron po reklamie jest ponawiane na kolejnym klipie.
Po awarii przeglądarki grafika jest czyszczona, a Chromium wraca po 3 s.
Gdy sidecar jest nieosiągalny, klient kończy pracę i pozycja leci bez grafiki.

## 7. Testy

```sh
npm test
```

Testy jednostkowe, test integracyjny z udawanym VizRS i klientem ErsatzTV
oraz prawdziwe przechwytywanie Chromium (pomijane bez przeglądarki).
