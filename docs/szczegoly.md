# Szczegóły: dostawcy, post-processing i pomiary

Notatki techniczne do [README](../README.md): skąd biorą się listy modeli, jak działa post-processing,
co zmierzono i jak działają okno ustawień, saldo i słownik. Daty i ceny są z dnia pomiaru.

## Dostawcy STT

| Dostawca | Streaming w trakcie mówienia | Model (domyślny) | Uwagi |
|---|---|---|---|
| **ElevenLabs** (domyślny) | tak, WebSocket, ręczny commit | `scribe_v2_realtime` (stream), `scribe_v2` (wsadowo) | najszybszy zmierzony |
| xAI | tak, WebSocket (binarne PCM) | `grok-voice-transcribe-2.0` | niezmierzony |
| OpenAI | tak, Realtime API (sesja transkrypcji, 24 kHz) | `gpt-live-transcribe` (stream), `gpt-transcribe` (wsadowo) | niezmierzony |
| OpenRouter | nie | `openai/gpt-transcribe` | jeden rachunek za STT i LLM |

Model wybierasz z listy na stronie „Rozpoznawanie mowy”, a „Wpisz inną nazwę…” przyjmuje dowolny. Skąd są listy:

- OpenRouter: publiczny katalog `GET /api/v1/models?output_modalities=transcription` (bez klucza), od najnowszych;
- OpenAI: `GET /v1/models` z twoim kluczem, tylko modele z `transcribe` albo `whisper` w nazwie; bez klucza lista wbudowana;
- ElevenLabs i xAI: lista wbudowana według dokumentacji z 15.10.2026, bo ich API nie podaje modeli STT (`/v1/models` w ElevenLabs zwraca tylko modele mowy syntetycznej).

Model domyślny ma dopisek „zalecany”, a model dodany do katalogu w ostatnich 45 dniach „nowy” z datą. Te same listy wypisuje `cargo run -p hyperrust-core --example catalog`.

Przy modelu jest też cena za godzinę mowy, a przy modelach rozliczanych za tokeny (Gemini 3.5 Transcribe, gpt-4o-transcribe przez OpenRouter) cena za 1M tokenów. OpenRouter podaje ceny w katalogu, zwykle za sekundę nagrania. Wyjątkiem są modele Microsoftu: ich cena jest za godzinę, co potwierdził koszt prawdziwego zapytania (6 s za 0,000167 $, czyli 0,10 $/h). Dla ElevenLabs, xAI i OpenAI ceny pochodzą z cenników dostawców z 15.10.2026 i są wpisane w aplikację. Tryb na żywo kosztuje więcej: ElevenLabs 0,39 zamiast 0,22 $/h, xAI 0,20 zamiast 0,10 $/h, OpenAI `gpt-live-transcribe` 1,02 zamiast 0,27 $/h. W abonamencie ElevenLabs płacisz kredytami: ok. 68 za minutę wsadowo i 78,5 na żywo. Na liście post-processingu przez OpenRouter jest cena za 1M tokenów wejścia i wyjścia (Haiku 5.5: 0,10 $ / 0,50 $). CLIProxy działa w abonamencie, więc tam cen nie ma.

Nowego dostawcę dodaje się, implementując `BatchProvider` (budowa zapytania i parsowanie odpowiedzi), a opcjonalnie też `LiveProtocol` (ramki WebSocket) w `src-tauri/crates/core/src/stt/`. Kształt zapytań ustalono z dokumentacji dostawców z 15.10.2026; parsowanie jest testowane na nagranych odpowiedziach (`tests/fixtures/`).

## Post-processing

| Wykonawca | Jak | Koszt |
|---|---|---|
| **OpenRouter** (domyślny), `anthropic/claude-haiku-5.5` | endpoint zgodny z OpenAI (`/chat/completions`, `stream: true`), klucz OpenRouter | płatne za tokeny |
| CLIProxy albo inny endpoint zgodny z OpenAI, `claude-haiku-5-5` | ten sam protokół pod twoim adresem, np. CLIProxy w sieci Tailscale | w ramach subskrypcji |
| `claude -p` / `codex exec` | uruchomienie CLI bez okna konsoli; `claude` bez `--bare`, bo ten tryb ignoruje logowanie subskrypcją | subskrypcja, ale ~5–6 s |

Transkrypcja trafia do modelu w znacznikach `<transkrypcja>`, a wynik jest wycinany ze znaczników `<wynik>`. Komentarz modelu poza znacznikami nigdy nie trafia do wklejanego tekstu, podobnie jak znaczniki, gdy model wyjątkowo odeśle samą transkrypcję.

Domyślny prompt (`AGENT_PROMPT` w `modes.rs`) jest redaktorem, a nie asystentem, i redaguje minimalnie: usuwa wypełniacze, powtórzenia i samopoprawki, poprawia interpunkcję, odmianę i błędy rozpoznawania, zwłaszcza nazwy techniczne („git czekaut” → `git checkout`). Słowa użytkownika, ich kolejność, ton i formę wypowiedzi zostawia („odpalam” zostaje, pytanie „czy możesz…” nie staje się poleceniem, „chyba” zostaje) i niczego nie dodaje. Poprzednia wersja (15.10) przepisywała dyktowanie na „staranny prompt”: zamieniała słowa na synonimy („ruszajmy dalej” → „kontynuujmy realizację”), pytania w polecenia, przestawiała zdania i dopisywała wnioski („więc może warto się tam wzorować”). Na 24 dyktowaniach z historii laptopa i 14 z `tests/fixtures/dictations.txt` (przykład `prompt_eval`) nowa wersja tego nie robi i nie skomentowała żadnej wypowiedzi bez polecenia. Drugi i trzeci przykład w prompcie pokazują pytanie i wypowiedź bez polecenia; bez przykładu takiej wypowiedzi Haiku w 5 z 9 prób zamieniał notatkę w polecenie albo ją komentował.

Kosztem jest czas: przy tym prompcie Haiku częściej myśli, więc mediana całości w naprzemiennych przebiegach przez CLIProxy wzrosła z 2,0–2,1 s do 3,0–3,3 s. Pomiary niżej dotyczą jeszcze poprzedniej wersji. `reasoning_effort: none` skraca czas do 1,3–1,5 s, ale wtedy model znów dopisuje treść („przycisk „Pokaż”, który go odsłania”), rozwija cytowany komunikat błędu i zostawia samopoprawki („Zrób commit, a nie, czekaj.”). Dlatego tryb agenta domyślnie myśli, a `none` możesz wpisać w ustawieniach, jeśli wolisz szybkość. Krótsza, bardziej stanowcza wersja promptu nie przyspieszyła (3,2 s).

**Dlaczego Haiku 5.5 przez CLIProxy:** to najszybszy z najnowszych modeli (Haiku 5.5, GPT-6 Luna), a CLIProxy działa na subskrypcjach, więc nie ma kosztu za tokeny. Z docelowym promptem CLIProxy jest tak samo szybki jak OpenRouter (mediana TTFT ~0,96 s wobec ~1,07 s, po 9 dyktowaniach). Model i wykonawcę zmienisz jednym polem w ustawieniach.

**GPT-6 Luna (`gpt-6-luna` w CLIProxy) jako alternatywa:** redaguje wierniej („sajz” → `size`, „chyba” zostaje, komunikat błędu cytuje bez rozwijania, ani razu nie odesłała samej transkrypcji), ale jest około 2× wolniejsza: na 12 dyktowaniach mediana całości to 3,0–3,3 s wobec 1,6 s dla Haiku, a mediana TTFT 1,4–1,7 s wobec 1,05 s. Najlepiej działa bez `reasoning_effort`; przy `low` przekręciła `clippy` na `klipi`, a przy `none` zgubiła ukośniki w nazwie gałęzi i zamieniła pytanie w polecenie.

**Co sprawdzono w CLIProxy (15.10, wersja 8.0.23):**

- `reasoning_effort` zostaje pusty. Adaptacyjne myślenie Haiku włączyło się w 1 z 36 zapytań (+~2 s TTFT). `none` usuwa te skoki, ale psuje redakcję: na trudnym wpisie Haiku odsyłał wtedy samą transkrypcję zamiast wyniku (5 z 6 prób, bez effortu 0 z 6). Sonnet 5.5 i Gemini 3.5 Flash Lite przez OpenRouter w ogóle odrzucają `none`.
- Wyjątek: wbudowany prompt porządkujący (tryby inne niż „Prompt dla agenta”, w tym „Surowa transkrypcja” z post-processingiem) idzie do Haiku z `reasoning_effort: none`, chyba że w ustawieniach wpiszesz inną wartość. Przy tym krótkim prompcie Haiku 5.5 myślał prawie zawsze: na 9 prawdziwych dyktowaniach z laptopa TTFT wynosił 2,5–9,4 s, a mediana całości 4,3 s. Z `none` mediana spadła do 1,6 s (CLIProxy) i 1,3 s (OpenRouter), bez błędów i bez gubienia treści. Haiku 4.5 nie myśli wcale (1,3 s), ale częściej zostawia wypełniacze („nie wiem”, „tak jakby”) i raz wstawił cyrylicę w słowo „antywirusie”.
- Cache promptu nie pomaga. Kamuflaż Claude Code wstawia nasz prompt systemowy za pierwszą wiadomością użytkownika i liczy nagłówek rozliczeniowy z jej treści, więc przy nowej transkrypcji cache jest tylko zapisywany. Kształt `prompt_cache_options: {"mode": "explicit"}` ze stałym wstępem trafia w cache (1804 z 1860 tokenów), ale TTFT nie spada (mediana 1,06 s wobec 0,94 s w 8 naprzemiennych parach), więc zapytanie zostaje proste.
- Połączenie do CLIProxy (HTTP przez Tailscale) zostaje w puli, a `HEAD` co 45 s dostaje 404 w ~1 ms bez klucza.
- Czas to głównie Anthropic: TTFT 0,8–1,2 s z rzadkimi skokami do 2–5,7 s (część to myślenie adaptacyjne), a przy długim dyktowaniu (~400 tokenów wyniku) samo generowanie trwa 1,5–1,9 s.

## Pomiary

Test dymny bez GUI na serwerze z Linuksem (ta sama ścieżka co w aplikacji, `examples/smoke.rs`): polski plik WAV (TTS, 12,35 s i 2,69 s) jest podawany do potoku w tempie mowy, po 20 ms. Koniec pliku oznacza „puszczenie klawisza”. Czasy liczone są od puszczenia. Wklejanie mierzone jest dopiero na Windows (`timings.jsonl`).

**STT, od puszczenia do gotowej transkrypcji (12,35 s mowy):**

| Wariant | Pomiary [ms] | Mediana |
|---|---|---|
| ElevenLabs `scribe_v2_realtime`, streaming | 206–402 (20 przebiegów) | ~250 ms |
| ElevenLabs realtime, krótkie 2,69 s | 197–296 (6) | ~240 ms |
| ElevenLabs `scribe_v2`, wsadowo | 892–1406 (6) | ~1250 ms |
| OpenRouter `openai/gpt-transcribe`, wsadowo | 963–1324 (6) | ~1020 ms |

**Post-processing, ten sam tekst (186 znaków), `examples/llm_bench.rs`, czas do pierwszego tokena / całość:**

| Model | Mediana TTFT | Mediana całości |
|---|---|---|
| CLIProxy `claude-haiku-5-5` | 1,07–1,99 s | 1,50–2,44 s (3 serie) |
| CLIProxy `gpt-6-luna` (effort `low` / brak) | 1,63 / 2,51 s | 2,74 / 3,24 s |
| CLIProxy `gpt-5.6-luna` | 1,35 s | 2,41 s |
| CLIProxy `gemini-3.1-flash-lite` | 1,01 s | 1,12 s |
| CLIProxy `gemini-3-flash` | 1,10 s | 1,20 s |
| OpenRouter `anthropic/claude-haiku-5.5` | 0,84–0,93 s | 1,35 s |
| OpenRouter `openai/gpt-6-luna` (effort brak) | 1,18 s | 1,83 s |
| OpenRouter `google/gemini-3.1-flash-lite` | 0,63 s | 0,76 s |
| `claude -p` (haiku) | – | 5,8 s |
| `codex exec` (`gpt-5.6-luna`) | – | 5,8 s |

Wolne i niestabilne były m.in. `zai-glm-5.3-flash` (9,6 s), `gpt-oss-120b` (14–46 s), `google/gemini-3.8-flash` (8,5 s).

Powtórka 15.10 po południu, `prompt_eval` na 12 dyktowaniach korpusu przez CLIProxy (mediana całości / najgorszy): `claude-haiku-5-5` 1,5 / 3,2 s, `gemini-3-flash` 1,8 / 6,3 s, `gemini-3.5-flash-lite` 1,8 / 2,5 s (ale na wpisie z historii jedno zapytanie trwało 86 s), `zai-glm-5.3-flash` 17 s. Z modeli w subskrypcji Haiku pozostaje najszybszy.

**Jakość i czas przez OpenRouter (15.10 wieczorem).** Test objął 23 dyktowania: 14 z korpusu i 9 z laptopa. Oceniał je ślepy sędzia Opus 5.5 przez CLIProxy w skali 0–10 (trzy przebiegi, rozrzut ±0,2), według wierności, oczyszczenia, polszczyzny i terminów. Mediana całości:

| Model (reasoning effort) | Ocena | Mediana |
|---|---|---|
| `deepseek/deepseek-v4.1-flash` (domyślny, myśli) | 8,9–9,1 | 2,6 s; pojedyncze zapytania 15–50 s |
| `deepseek/deepseek-v4.1-flash:nitro`, `none` | 8,4 | **0,40 s** |
| `qwen/qwen3.8-flash`, `none` | 8,1–8,2 | 1,8 s (z myśleniem 15–20 s) |
| `openai/gpt-6-luna` (domyślny) | 8,2–8,3 | 3,3 s |
| `deepseek/deepseek-v4.1-flash`, `none` | 7,8–8,1 | 1,1 s |
| `anthropic/claude-haiku-5.5` (domyślny) | 7,6–7,7 | 1,3 s |
| `google/gemini-3.1-flash-lite` | 7,4–7,7 | 0,8 s |
| CLIProxy `claude-haiku-5-5` (domyślny w aplikacji) | 6,9–7,0 | 1,8 s |

Czas DeepSeeka zależy od myślenia, a nie od dostawcy. Ten sam dostawca (DigitalOcean) odpowiadał w 0,6–0,8 s bez tokenów myślenia, a z nimi w 2–15 s. Przy `none` dopisek `:nitro` (dostawcy sortowani po przepustowości) kierował wszystko do Together: mediana 0,47 s wobec 0,80 s przy domyślnym routingu, a koszt to ok. 0,15 $ za 1000 dyktowań. Tryb `fast` (`service_tier`) ma dla tego modelu tylko BaseTen, za podwójną cenę, i w 22 z 28 zapytań odesłał je do zwykłego dostawcy.

Żeby tak ustawić aplikację: na stronie „Post-processing” wybierz OpenRouter, w polu „Wpisz inną nazwę…” wpisz `deepseek/deepseek-v4.1-flash:nitro`, a w „Zaawansowanych” ustaw reasoning effort `none`. Słabszy punkt tej konfiguracji: raz na kilkanaście dyktowań zostawia samopoprawkę albo wypełniacz „no”. Haiku przez CLIProxy częściej zostawia samopoprawki i „yy” oraz przepisuje zdania.

**Całość, od puszczenia klawisza do tekstu gotowego do wklejenia (12,35 s mowy):**

| Konfiguracja | Pomiary |
|---|---|
| ElevenLabs realtime + CLIProxy Haiku 5.5 (domyślna) | 1,27–3,37 s, mediana 1,44 s (11 przebiegów) |
| ElevenLabs realtime + OpenRouter Haiku 5.5 | 1,49–1,75 s |
| ElevenLabs realtime + OpenRouter Gemini 3.1 Flash Lite | 0,96–1,18 s |
| OpenRouter STT + OpenRouter Haiku 5.5 | 2,23–2,49 s |
| ElevenLabs realtime, tryb „Surowa transkrypcja” | 0,21–0,30 s |

Pomiary wykonano na serwerze z Linuksem. Z Windows CLIProxy jest wołany przez Tailscale, co w sieci lokalnej dodaje kilka milisekund.

Pierwsze dyktowania na Windows (laptop, build `d119b64`, 2 dyktowania po ~5 s): transkrypcja gotowa 0,24–0,27 s po puszczeniu klawisza, wklejenie 4 ms, post-processing Haiku 2,1 i 3,3 s (TTFT 1,27 i 3,18 s), całość 2,4 i 3,6 s. Prawie cały czas po puszczeniu klawisza to więc model.

## OpenRouter: czy płacić w jednym miejscu?

OpenRouter obsługuje transkrypcję osobnym endpointem `POST /api/v1/audio/transcriptions`: JSON z audio w base64, bez streamingu. Modele to m.in. `openai/gpt-transcribe`, `elevenlabs/scribe-v2`, `mistralai/voxtral-mini-transcribe` i `x-ai/grok-stt-1.0`. Zmierzone `openai/gpt-transcribe` daje dobrą polszczyznę i interpunkcję, kosztuje ok. 0,001 USD za 12 s nagrania (~0,28 USD za godzinę mowy). Ponieważ jednak całe nagranie jest wysyłane dopiero po puszczeniu klawisza, kosztuje ~1 s czekania przy 12 s mowy, a opóźnienie rośnie z długością nagrania.

**Rekomendacja:** do codziennego dyktowania ElevenLabs realtime, bo jest ~4× szybszy po puszczeniu klawisza, plus post-processing przez CLIProxy (subskrypcja). OpenRouter ma sens, gdy jeden rachunek jest ważniejszy niż ~1 s: ustaw go wtedy jako dostawcę STT i jako wykonawcę post-processingu, a wystarczy jeden klucz.

**Inne firmy przez OpenRouter.** Katalog z 15.10.2026 ma 26 modeli STT, m.in. Grok (`x-ai/grok-stt-1.0`), Gemini 3.5 Transcribe (`google/gemini-3.5-transcribe`), Microsoft MAI-Transcribe 2 i 1.5 (`microsoft/mai-transcribe-2`), Meta Muse Voice Transcribe, AssemblyAI, Mistral Voxtral, Deepgram Nova 3, Qwen i NVIDIA. Test dymny tą samą ścieżką co w aplikacji (`smoke --provider openrouter --batch --stt-model …`, polskie nagranie testowe), czas od końca nagrania do transkrypcji:

| Model | Wynik | Cena za godzinę mowy |
|---|---|---|
| `microsoft/mai-transcribe-2` | 1,04 s, bez błędów | 0,10 $ |
| `elevenlabs/scribe-v2` | 1,08 s, bez błędów | 0,11 $ |
| `x-ai/grok-stt-1.0` | 1,18 s, bez błędów | 0,10 $ |
| `openai/gpt-transcribe` | 1,19 s, bez błędów | 0,27 $ |
| `meta/muse-voice-transcribe-1.0` | 2,29 s, jedno słowo źle | 0,18 $ |
| `google/gemini-3.5-transcribe` | 5,09 s, bez błędów | za tokeny (2 $ / 12 $ za 1M) |
| `assemblyai/universal-3-6-pro` | błąd 400 od dostawcy (przyczyna niezbadana) | 0,30 $ |

Przez OpenRouter każdy z nich działa tylko wsadowo, więc po puszczeniu klawisza czekasz ~1 s zamiast ~0,25 s. Grok bezpośrednio u xAI (`grok-voice-transcribe-2.0`) działa też na żywo, ale wymaga klucza xAI.

Potwierdza to laptop (15.10 wieczorem). `microsoft/mai-transcribe-2` dawał tekst po 1,0–1,4 s od puszczenia klawisza (3 dyktowania po 4,9–18,7 s), a ElevenLabs realtime tego samego dnia po 0,18–0,31 s (22 dyktowania). Z serwera ten sam model odpowiada w 0,5–1,1 s (mediana 0,59 s na 18 nagraniach), więc różnica to głównie wysłanie nagrania z laptopa. MAI-Transcribe 2 jest najszybszy wśród modeli wsadowych, ale nie dogoni rozpoznawania na żywo. W polskim teście tych 18 nagrań (głosy syntetyczne, 6 poleceń z terminami technicznymi) odsetek błędnych słów wyniósł: MAI-Transcribe 2 0,2%, Scribe v2 0%, Grok 1,4%, `gpt-transcribe` 1,5%.

## Okno ustawień i saldo

Okno ustawień otwiera się kliknięciem ikony w zasobniku. Panel boczny ma dwie grupy:

- **Przegląd**, **Tryby**, **Historia** i **Słownik** — to, czego używasz na co dzień;
- **Ustawienia**: Skróty, Dźwięk i mikrofon, Wklejanie, Rozpoznawanie mowy, Post-processing, Klucze API i Ogólne (autostart, aktualizacje, historia, logi).

Każda strona to lista wierszy: opis po lewej, przełącznik albo pole po prawej. Wartości techniczne (limity czasu, adresy serwerów, bufor mikrofonu) są zwinięte pod „Zaawansowane”. Strona „Rozpoznawanie mowy” pokazuje listy modeli tylko wybranego dostawcy, a obok dostawcy STT i wykonawcy post-processingu jest plakietka „klucz zapisany” / „brak klucza”, która prowadzi do „Kluczy API”. Na dole jest jeden przycisk „Zapisz”, aktywny tylko wtedy, gdy coś zmieniłeś.

Strona „Przegląd” ma kafelki z bieżącym skrótem, trybem, rozpoznawaniem mowy i post-processingiem (kliknięcie prowadzi do ich strony) i pokazuje poniższe informacje. Karty sald dotyczą tylko dostawców z zapisanych ustawień: ElevenLabs, gdy rozpoznaje mowę; OpenRouter, gdy rozpoznaje mowę albo robi post-processing w którymś trybie; CLIProxy, gdy robi post-processing. Po zapisaniu zmiany dostawcy przegląd odświeża się sam.

- **ElevenLabs**: pozostałe kredyty, limit i plan oraz datę odnowienia (`GET /v1/user/subscription`). Szacuje też minuty, na które starczą kredyty: ok. 78,5 kredytu za minutę streamingu i 68 za minutę wsadowo, według analityki konta z 15.10.2026 (`SCRIBE_*_CREDITS_PER_MIN` w `balance.rs`). Na koniec liczy tempo konta w bieżącym okresie i mówi, czy kredytów wystarczy do odnowienia. Z kluczem zapasowym obok jest druga karta, „ElevenLabs — zapas”.
- **OpenRouter**: kwotę „do wydania”, czyli mniejszą z dwóch: saldo konta (`/credits`) albo pozostały limit klucza (`/key`). Pokazuje też wydatki z 7 dni i szacuje, na ile dni starczy przy tym tempie.
- **CLIProxy**: tylko model i liczbę zapytań. To subskrypcja, więc nie ma salda w API.
- **Nowe modele** dodane w ostatnich 45 dniach, po 8 najnowszych. Rozpoznawanie mowy pochodzi z katalogu OpenRouter, który obejmuje modele wielu firm, także tych, z których korzystasz bezpośrednio. Post-processing pochodzi z listy `/models` CLIProxy albo, przy OpenRouter, z jego katalogu: modele czatu wszystkich firm, bez modeli obrazów, wariantów (`:free`), aliasów (`~…-latest`), routerów (`openrouter/auto`, `openrouter/free`) i narzędzi bez ceny wyjścia (`scaledown/*`). Dopisek „nowszy niż twój” dostaje model tej samej firmy dodany później niż twój; przyrostek routingu, np. `:nitro`, nie zmienia tego porównania. CLIProxy podaje datę dodania modelu do serwera, nie premiery, więc starszy model bywa tam „nowy”. Przy OpenRouter obok przetestowanych modeli jest ocena i czas z [rankingu](wlasny-serwer.md#ranking-modeli-post-processingu) (tylko w buildzie z własnym serwerem).
- **Zużycie na tym komputerze**: dane z `timings.jsonl` z 7 i 30 dni (liczba dyktowań, minuty nagrań, mediana czasu do wklejenia, nieudane) oraz wykres minut z każdego dnia.
- Ostatnie dyktowania z czasami etapów i kosztem. Koszt to suma części: kwota, którą podał dostawca (OpenRouter zwraca `usage.cost` przy transkrypcji i w ostatniej porcji strumienia post-processingu), albo „≈” — szacunek z wbudowanego cennika STT razy długość nagrania (ElevenLabs, xAI, OpenAI; w abonamencie ElevenLabs płacisz kredytami, więc to tylko równowartość). „abon.” oznacza post-processing w abonamencie (CLIProxy, CLI), „?” część bez ceny. Po najechaniu widać podział. Ten sam koszt jest przy wpisach na stronie „Historia” i w `timings.jsonl` (`stt_cost_usd`, `stt_cost_estimated`, `llm_cost_usd`). Przykład z 15.10.2026 dla 2,7 s nagrania: MAI-Transcribe 2 przez OpenRouter 0,000083 $, ElevenLabs na żywo ≈0,00029 $, DeepSeek V4.1 Flash `:nitro` 0,00009 $.

xAI i OpenAI nie podają salda zwykłym kluczem API. Klucz trafia wyłącznie do swojego dostawcy. Te same liczby wypisuje `ELEVENLABS_API_KEY=… OPENROUTER_API_KEY=… cargo run -p hyperrust-core --example balance`.

**Saldo w dymku ikony w zasobniku.** Po najechaniu kursorem na ikonę HyperRust dymek pokazuje, oprócz trybu, saldo używanych dostawców (według tych samych zasad co przegląd), do których masz klucz, na przykład:

```
HyperRust — tryb: Prompt dla agenta
ElevenLabs: 64%, ≈8 h 30 min, odnowienie za 18 dni
OpenRouter: 3,21 $ (≈32 dni)
```

ElevenLabs pokazuje procent pozostałych kredytów, przybliżony czas mowy (stawka streamingu albo wsadowa, zależnie od ustawienia) i czas do odnowienia. Z kluczem zapasowym wiersz ma procent obu kont i łączny czas mowy, np. `ElevenLabs: 0% + 100%, ≈2 h 5 min`. OpenRouter pokazuje kwotę do wydania i liczbę dni przy tempie z ostatniego tygodnia. Znak ⚠ oznacza, że czas doładować: przy ElevenLabs zostało mniej niż 10% (przy dwóch kontach: łącznie) albo przy obecnym tempie kredyty skończą się przed odnowieniem, a przy OpenRouter jest mniej niż 1 $ albo starczy na mniej niż tydzień. Windows mieści w dymku 127 znaków, więc skrót i wersja dochodzą tylko wtedy, gdy zostaje miejsce.

Saldo nie jest odpytywane cyklicznie. Aplikacja pobiera je 5 s po starcie, po najechaniu na ikonę (gdy dane mają ponad minutę albo ostatnie pobranie się nie udało; nowe liczby widać przy tym samym albo następnym najechaniu), po zapisaniu lub usunięciu klucza, po przejściu na dostawcę z saldem oraz przy otwarciu przeglądu i przycisku „Odśwież”. Salda nieużywanych dostawców nie są pobierane, a ich wiersz znika z dymku od razu po zapisaniu ustawień.

## Słownik

Strona „Słownik” zbiera nazwy, które rozpoznawanie mowy przekręca, po jednej w linii: `Claude Code = Cloud Code, CloudCode`. Słownik działa na trzech poziomach:

- **Rozpoznawanie**: przy ElevenLabs poprawne nazwy idą jako `keyterms` (na żywo w adresie WebSocketu, wsadowo jako pola formularza; najwyżej 50 nazw, każda do 5 słów). Dopłata wynosi 20% w trybie na żywo i 0,05 $/h wsadowo, a szacunek kosztu ją uwzględnia. Pozostali dostawcy słownik na tym etapie pomijają.
- **Post-processing**: lista nazw z błędnymi formami jest doklejana do promptu systemowego każdego trybu z AI, więc model poprawia też formy odmienione i nieznane słownikowi. `prompt_sha` w historii liczy się z samego promptu trybu, bez słownika.
- **Wklejany tekst**: formy po „=” są zamieniane na poprawną nazwę zawsze, także w trybie bez AI i po nieudanym post-processingu. Zamiana obejmuje całe słowa w dowolnej wielkości liter i zachowuje końcówkę do 3 liter, a po nazwie pisanej wielkimi literami dodaje łącznik („Kodeksa” → „Codexa”, „JSONem” → „JSON-em”). Tekst w `backtickach`, adresy i ścieżki („example.com”) zostają bez zmian.

Startowe wpisy pochodzą z historii prawdziwych dyktowań: Scribe zapisywał Codexa jako „kodeks” i „Kodeksa”, Claude Code jako „Cloud Code”, a HyperRust jako „Hyper Rust”, i post-processing tego nie poprawiał. Haiku przerabiał też nieznane nazwy na inne: „EM klient” na „Anthropic”, „sentry” na „Sentre”. Pojedyncze „Cloud” nie jest w słowniku, bo pada też w „Google Cloud”. Ustawienia zapisane przed dodaniem słownika dostają wpisy startowe; wyczyszczony słownik zostaje pusty.
