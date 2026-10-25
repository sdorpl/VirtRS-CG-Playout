# PLAN: płynność animacji grafiki (ticker)

> Status (2026-10-25): **analiza, nic nie zmierzone ani zaimplementowane.** Wnioski z przeglądu kodu (`src/capture.rs`, `src/stream.rs`) i DOKUMENTACJA §1, §4, §8.
> Powiązane: [DOKUMENTACJA.md](../DOKUMENTACJA.md) §1 (Płynność animacji, Klatkaż kanału), §8 (benchmark).

## 0. Cel

Przewijany ticker i inne ciągłe animacje mają się poruszać równymi krokami w każdej klatce wyjścia: 25 i 50 kl./s przy materiałach europejskich, bez dokładania CPU (Ampere A1, 2 CPU sidecara).

## 1. Diagnoza: skąd szarpanie

### 1.1 Dudnienie częstotliwości Chromium i kanału (główna przyczyna)

Chromium składa stronę w 60 Hz (`BROWSER_FPS`). Screencast bierze co n-tą klatkę (`every_nth_frame`, `src/capture.rs`). Klatka wyjścia pokazuje ostatnią klatkę Chromium sprzed swojej chwili (minus 100 ms):

| Kanał | Przechwytywanie | Na 1 s | Skutek |
|---|---|---|---|
| 25 kl./s (też A1 z `VIZRS_MAX_FPS=25`) | co 2. = 30/s | 6 źródła na 5 wyjścia | podwójny krok 5×/s |
| 50 kl./s (`VIZRS_FFMPEG_FPS=50`) | każda = 60/s | 6 na 5 | podwójny krok 10×/s |
| 30 kl./s | co 2. = 30/s | 1:1 | równo |
| 60 kl./s | każda = 60/s | 1:1 | równo |

To zachowanie deterministyczne, niezależne od obciążenia. Przy równym przewijaniu tekstu widać je najbardziej.

### 1.2 Koszt klatki przy ciągłej animacji

Każda klatka tickera jest nowa. Chromium koduje PNG **całego okna**, bo screencast nie ma przycinania. Sidecar ją dekoduje, wyznacza prostokąty, skaluje (`render_size`) i robi `yuva420p`. Pod limitem CPU klatki wypadają (pomiary A1, DOKUMENTACJA §4). To druga przyczyna, ważna głównie na A1.

## 2. Rozważone kierunki

### 2.1 Ticker w osobnym procesie (osobna przeglądarka) – nie teraz

Sam osobny proces nie usuwa dudnienia z 1.1, bo każda przeglądarka ma te same 60 Hz. Pomaga tylko na koszt z 1.2 i tylko wtedy, gdy ticker ma własne, małe okno (np. 1920×80):

- plus: kilkanaście razy mniej pikseli PNG na klatkę; stojąca reszta grafiki idzie jako `Repeat`; praca dzieli się na rdzenie;
- warunek: VizRS musi wystawiać `/output` z jedną warstwą albo elementem. Integracja idzie tylko przez protokół; sprawdzić w `../VizRS CG System/docs/API.md`;
- minus: rozjazd wejść i wyjść grafik między dwiema stronami (dwa zegary, różne opóźnienia); mieszanie alfy dwóch źródeł w sidecarze (pakiety ETV i `yuva420p`; dziś jedna przeglądarka na klucz rozmiaru, `--renderer-process-limit=1`); ok. 150–250 MB RAM i stały koszt drugiej przeglądarki przy 2 CPU na A1.

Wracamy do tego dopiero, gdy po etapie 2 (BeginFrame) na A1 nadal brakuje CPU.

### 2.2 Klatkaż kanału 30 albo 60 kl./s – tylko przy takich materiałach

Przy 30/60 kl./s grafika idzie 1:1 (tabela 1.1). ErsatzTV bierze jednak klatkaż z materiałów, a kanał zmienia tylko `VIZRS_FFMPEG_FPS` (nakładka ffmpeg). Przy materiałach 25 kl./s:

- 30 kl./s: co piąta klatka filmu powtórzona, więc szarpanie przechodzi z grafiki na wideo;
- 60 kl./s: klatki filmu na przemian po 2 i 3 klatki wyjścia (60/25 = 2,4), ruch w materiale nierówny.

Do sprawdzenia także przy 30/60: faza (zegar Chromium nie jest zgrany z klatkami wyjścia; przy rozrzucie znaczników czasu możliwy krok podwójny i zaraz zerowy) i dryf (Chromium np. 59,9 Hz zamiast 60 albo 29,97 wobec 30 daje przeskok co kilka do kilkudziesięciu sekund).

Koszt: 30 kl./s prawie jak dziś (20% więcej klatek dla kodera). 60 kl./s to dwa razy więcej PNG (1080p50 na Xeonie ok. 170% CPU sidecara), ok. 310 MB/s surowych klatek 1080p i dwa razy więcej klatek dla kodera; na A1 nierealne. Na A1 z `VIZRS_MAX_FPS=25` kanał 60 kl./s dostaje każdą klatkę grafiki równo podwojoną (ruch równy, ale 30 Hz), płaci głównie x264.

**Wniosek:** materiały 30/60 kl./s → `VIZRS_FFMPEG_FPS=30` (albo 60, gdy CPU pozwala), opisać w DOKUMENTACJA §1. Materiały 25 kl./s → zostać przy 25/50 i robić etap 2.

### 2.3 Klatki Chromium sterowane przez sidecar (BeginFrame) – rekomendowane

Chromium rysuje klatkę dokładnie w chwili klatki wyjścia (25 albo 50 Hz). Dudnienie z 1.1 znika, a klatek niepotrzebnych kanałowi nie ma, więc i CPU spada. Stary benchmark (`chromium-frames`, DOKUMENTACJA §8, commit 0c54143) mierzył już: przy 720p 99% równych kroków zamiast 65%, CPU 130% wobec 149%. Przy 1080p na Xeonie wyszło 71–92%, bo procesor nie nadążał z PNG w każdym slocie. Obraz ma `chromium-headless-shell`, który obsługuje `--enable-begin-frame-control`. Nie trzeba niczego zmieniać w VizRS.

Znane pułapki (DOKUMENTACJA §8): z `--disable-new-content-rendering-timeout` każda klatka zgłasza zmianę; żądanie zrzutu zawsze ją zgłasza, a klatka po zrzucie też (pochłania ją pusta klatka zaraz po zrzucie); na stojącej stronie co sekundę idzie zrzut kontrolny.

### 2.4 Ticker przewijany przez sidecar – pomysł na później

Chromium rysuje raz cały pasek tekstu, a sidecar przesuwa go o piksele wyliczone z czasu klatki wyjścia. Płynność idealna przy minimalnym CPU. To jednak duża zmiana projektu: VizRS musiałby podawać przez protokół treść, tempo i stan tickera, a część grafiki przestałaby pochodzić z `/output`.

## 3. Etapy

1. **Miara równych kroków w benchmarku.** `bench/sidecar.mjs` podaje dziś tylko `pictures_per_s`, więc dudnienia nie pokaże. Trzeba dodać odsetek klatek wyjścia, w których ticker przesunął się o jeden krok (wzorzec: stary `bench/bench.mjs`, commit 0c54143). Zmierzyć stan obecny: 25, 30, 50 i 60 kl./s, x86 i A1, 720p i 1080p. Przy okazji sprawdzić rzeczywistą częstotliwość Chromium (60 Hz, dryf).
2. **Przechwytywanie BeginFrame za opcją** (np. `capture_mode`; `src/config.rs`, `config.example.json`, `docker-compose.yml`, DOKUMENTACJA §5, test). Domyślnie wyłączone, dopóki pomiary z etapu 1 nie potwierdzą zysku na A1. Obsłużyć pułapki z 2.3, zrzut stanu końcowego, pauzę bez sesji i restart po awarii (zasada „grafika nigdy nie zatrzymuje emisji”).
3. **Decyzja o domyślnym trybie** na podstawie pomiarów (x86 i A1). Aktualizacja DOKUMENTACJA §1 (Płynność animacji) i §4 (A1).
4. **Opis klatkażu 30/60** w DOKUMENTACJA §1 (Klatkaż kanału): kiedy pomaga, a kiedy szkodzi (2.2).
5. **Osobny proces na ticker (2.1)** tylko wtedy, gdy po etapie 3 na A1 brakuje CPU i VizRS umie wystawić pojedynczą warstwę.
