# Ręczny test na Windows

Tych elementów nie da się sprawdzić z Linuksa (CI i test dymny pokrywają tylko rdzeń, STT i post-processing). Przejdź listę po każdej większej zmianie w `src-tauri/src/`. Czasy każdego dyktowania są w `%LOCALAPPDATA%\HyperRust\logs\timings.jsonl` oraz na stronie „Przegląd” w ustawieniach (sekcja „Czasy ostatnich dyktowań”).

## Start i ustawienia

- [ ] `HyperRust.exe` uruchamia się bez konsoli; w zasobniku pojawia się ikona HyperRust (znak „>” z falą dźwięku na pomarańczowo-malinowym tle) z podpowiedzią „HyperRust — Ctrl+Shift+Space …”.
- [ ] Pierwsze uruchomienie (bez `%APPDATA%\HyperRust\settings.json`) albo brak klucza STT otwiera okno ustawień.
- [ ] Klucze: wpisz klucz ElevenLabs i token CLIProxy, „Zapisz” (albo Enter w polu) → odznaka „zapisany”. W Menedżerze poświadczeń Windows (Poświadczenia systemu Windows → Poświadczenia ogólne) są wpisy `elevenlabs.HyperRust` i `llm-endpoint.HyperRust`; w `settings.json` nie ma żadnego klucza. Plakietki przy dostawcy na stronach „Rozpoznawanie mowy” i „Post-processing” zmieniają się z „brak klucza” na „klucz zapisany” bez ponownego otwierania okna.
- [ ] Zapasowe konto ElevenLabs: zapisz klucz drugiego konta w „ElevenLabs — konto zapasowe” (wpis `elevenlabs-backup.HyperRust` w Menedżerze poświadczeń). „Przegląd” ma drugą kartę „ElevenLabs — zapas”, a dymek zasobnika wiersz „ElevenLabs: X% + Y%, ≈…”. Potem w polu głównego klucza zapisz błędny klucz (np. `sk_zly`) i podyktuj: tekst wkleja się normalnie, wskaźnik pokazuje „Wklejono · klucz zapasowy”, a w `hyperrust.log` jest „przełączono na klucz zapasowy”. Następne dyktowanie idzie od razu na żywo zapasowym kluczem (w `timings.jsonl` `streamed: true`). Na koniec przywróć prawdziwy klucz główny: kolejne dyktowanie znów idzie głównym kluczem, bez komunikatu o zapasowym.
- [ ] Strona „Post-processing”: lista „Model” sama pobiera modele z CLIProxy (`GET /v1/models`, Tailscale musi działać), grupuje je (Claude, Gemini, GPT, pozostałe), oznacza `claude-haiku-5-5` jako zalecany i pokazuje zapisany model. Wybór innego modelu włącza „Niezapisane zmiany”. „Wpisz inną nazwę…” pokazuje pole „Nazwa modelu”. Przy wyłączonym Tailscale pod listą jest komunikat błędu, a zapisany model nadal jest wybrany.
- [ ] Język: nowa instalacja startuje po angielsku (okno, menu zasobnika, wskaźnik), a `settings.json` sprzed tej wersji (bez `ui_language`) zostaje po polsku. „Ogólne” → „Language / Język” przełącza okno od razu, bez zapisu; po „Zapisz” menu i dymek zasobnika, wskaźnik przy następnym dyktowaniu, komunikaty błędów i stan aktualizacji są w nowym języku, a wbudowane tryby z niezmienioną nazwą zmieniają nazwę („Prompt dla agenta” ↔ „Agent prompt”). Prompty i słownik zostają bez zmian.
- [ ] Drugie uruchomienie EXE nie tworzy drugiej instancji, tylko otwiera ustawienia.
- [ ] Lewy klik ikony HyperRust w zasobniku, gdy aktywne jest inne okno (np. przeglądarka): okno ustawień otwiera się na wierzchu i ma fokus, a nie tylko miga na pasku zadań. To samo, gdy okno ustawień jest już otwarte pod innymi oknami albo zminimalizowane.
- [ ] „Historia…” w menu zasobnika otwiera okno od razu na stronie „Historia”; gdy okno jest już otwarte na innej stronie, przełącza je na „Historię” i wyciąga na wierzch.
- [ ] Zamknięcie okna ustawień nie kończy aplikacji; „Zakończ” w menu zasobnika kończy.
- [ ] Okno otwiera się na stronie „Przegląd”. Kliknięcie kafelka (Skrót, Tryb, Mowa → tekst, Post-processing) przenosi na jego stronę. Karta ElevenLabs pokazuje kredyty i datę odnowienia zgodne z panelem ElevenLabs, a karta OpenRouter saldo zgodne z openrouter.ai/settings/credits. Bez klucza karta pokazuje „brak klucza”, a link „Dodaj klucz” przenosi na stronę „Klucze API”. Po zapisaniu klucza saldo odświeża się samo.
- [ ] Najechanie kursorem na ikonę w zasobniku (kilka sekund po starcie) → dymek: „HyperRust — tryb: …”, a pod spodem wiersze ElevenLabs (procent, ≈czas mowy, odnowienie za …) i OpenRouter (kwota $, ≈dni), z liczbami zgodnymi z „Przeglądem”. Dostawca bez klucza nie ma wiersza. Po podyktowaniu dłuższego tekstu i ponownym najechaniu po minucie procent ElevenLabs spada. Bez sieci wiersz brzmi „nie udało się sprawdzić salda”, a po przywróceniu sieci kolejne najechanie pokazuje liczby. Tekst dymka nie jest ucięty w połowie słowa (gdy nie ma miejsca, znika wiersz skrótu i wersji).
- [ ] Po kilku dyktowaniach „Odśwież” zwiększa liczby w „Zużyciu na tym komputerze”, a dzisiejszy słupek wykresu rośnie (podpowiedź po najechaniu: minuty i liczba dyktowań).
- [ ] Rozpoznawanie mowy przełączone na OpenRouter, post-processing na CLIProxy → po „Zapisz” przegląd pokazuje karty OpenRouter i CLIProxy, bez ElevenLabs, a dymek ikony nie ma wiersza ElevenLabs. Powrót na ElevenLabs przy post-processingu przez CLIProxy → znika OpenRouter, wraca ElevenLabs (w dymku po chwili). xAI albo OpenAI przy CLI jako wykonawcy → brak kart sald i notka, że wybrani dostawcy nie mają salda.
- [ ] Zmiana dowolnego pola na innej stronie → w stopce „Niezapisane zmiany” i aktywny „Zapisz”; po zapisie stopka wraca do „Zapisano.”, a kafelki przeglądu pokazują nowe wartości. Zmiana trybu z zasobnika nie zgłasza niezapisanych zmian, a na stronie „Tryby” od razu zaznacza ten tryb.
- [ ] „Zaawansowane” na każdej stronie jest zwinięte. Na „Rozpoznawanie mowy” modele wybranego dostawcy są listami w głównej karcie (ElevenLabs: na żywo i wsadowy; xAI: jeden), a zmiana dostawcy od razu podmienia listy. OpenRouter: lista ma ~26 modeli wielu firm, od najnowszych, nowe z dopiskiem „nowy, data”, `openai/gpt-transcribe` z dopiskiem „zalecany”. Wybór innego modelu włącza „Niezapisane zmiany”, a po zapisie i dyktowaniu `timings.jsonl` ma ten model. „Wpisz inną nazwę…” pokazuje pole nazwy. Bez sieci pod kartą jest komunikat, a zapisany model nadal jest wybrany.
- [ ] Ceny na listach: ElevenLabs `scribe_v2 · 0,22 $/h · zalecany` i na żywo `0,39 $/h`, pod kartą informacja o kredytach. OpenRouter: przy każdym modelu cena za godzinę (np. MAI-Transcribe 2 `0,10 $/h`, GPT Transcribe `0,27 $/h`) albo za 1M tokenów (Gemini 3.5 Transcribe). Post-processing przez OpenRouter: `anthropic/claude-haiku-5.5 · 0,10 $ / 0,50 $ za 1M tokenów · zalecany`. Na liście CLIProxy nie ma cen.
- [ ] „Przegląd” → „Nowe modele”: karta „Rozpoznawanie mowy” (OpenRouter) i „Post-processing” (CLIProxy) z datami z ostatnich 45 dni, bez modelu, którego używasz. „Zmień model” przenosi na stronę dostawcy. Bez Tailscale karta CLIProxy pokazuje błąd, a karta STT nadal listę.
- [ ] Post-processing przez OpenRouter z `deepseek/deepseek-v4.1-flash:nitro`: karta „Post-processing” ma do 8 modeli różnych firm (nie tylko DeepSeek), z cenami, bez modeli `:free`, `~…`, `openrouter/auto` i `scaledown/*`, i bez twojego modelu. DeepSeek V4.1 Flash nie jest oznaczony jako „nowszy niż twój”.
- [ ] „Czasy ostatnich dyktowań” mieszczą się w oknie o domyślnej szerokości, ostatnia kolumna to „Koszt”. ElevenLabs + CLIProxy: `≈0,00… $ + abon.`; OpenRouter STT i post-processing: kwota bez „≈”, a po najechaniu podział na dwie kwoty „z odpowiedzi dostawcy”. Po najechaniu na czas w kolumnie „LLM” widać model. Ten sam koszt jest przy nowych wpisach na stronie „Historia”, a starsze wpisy go nie mają.
- [ ] Wydanie z GitHuba: „Post-processing” → „Ranking modeli” pokazuje tabelę z GitHuba z dzisiejszą datą, a po najechaniu na ocenę nie ma uwag sędziego.
- [ ] Build z własnym serwerem: „Post-processing” → „Ranking modeli”: tabela z serwera (ocena, czas, koszt za 1000 dyktowań, data testu) mieści się w oknie; twój model ma plakietkę „twój” i przycisk „Używasz”. Po najechaniu na ocenę widać uwagi sędziego, na czas — 90. percentyl i wcześniejsze pomiary. „Użyj” przy innym modelu przełącza wykonawcę na OpenRouter, wpisuje model z `:nitro` i reasoning effort, a „Zapisz” staje się aktywny. Notka pod tabelą podaje datę rankingu; bez Tailscale zamiast tabeli jest błąd pobierania.
- [ ] Przy post-processingu przez OpenRouter karta „Nowe modele” → „Post-processing” pokazuje przy przetestowanych modelach ocenę i czas z rankingu (np. `8,4 · 0,4 s`), a przy modelach bez oceny powód: „przerwany” albo „niedostępny” (szczegóły po najechaniu).
- [ ] Autostart z menu zasobnika: „Uruchamiaj z Windows” → znaczek przy pozycji, w Menedżerze zadań → Aplikacje autostartu jest „HyperRust” (włączone), a otwarte okno ustawień od razu pokazuje włączony przełącznik „Uruchamiaj z Windows” na stronie „Ogólne” (bez „Niezapisanych zmian”). Po wylogowaniu i zalogowaniu aplikacja startuje sama. Ponowne kliknięcie usuwa wpis.
- [ ] Wyłącz HyperRust w Menedżerze zadań (Aplikacje autostartu) → po ponownym otwarciu ustawień przełącznik na stronie „Ogólne” jest wyłączony, a w zasobniku nie ma znaczka. Włączenie przełącznika i „Zapisz” (albo pozycja w zasobniku) przywraca „Włączone” w Menedżerze zadań.

## Skrót (hook klawiatury)

- [ ] Tryb „Przytrzymaj albo stuknij dwa razy”: przytrzymaj Ctrl+Shift+Space, powiedz zdanie, puść → tekst wkleja się w Notatniku. Spacja nie trafia do Notatnika.
- [ ] Krótkie stuknięcie (<250 ms) bez drugiego → nic się nie wkleja, wskaźnik znika po ~0,5 s.
- [ ] Dwa szybkie stuknięcia → nagrywanie bez trzymania; kolejne naciśnięcie kończy i wkleja.
- [ ] Escape w trakcie nagrania anuluje (nic się nie wkleja, Escape nie trafia do aplikacji).
- [ ] Zmiana skrótu na stronie „Skróty” („Zmień”, np. sam RightCtrl) działa po „Zapisz” bez restartu; w trakcie przechwytywania skrót nie startuje nagrania.
- [ ] Skrót z samym RightCtrl: RightCtrl+C w innej aplikacji nadal kopiuje (wciśnięcie innego klawisza anuluje nagranie).
- [ ] Po zablokowaniu stacji (Win+L) i odblokowaniu skrót działa (także sam RightCtrl), modyfikatory się nie „zawieszają”.
- [ ] „Zmień” zatwierdzony Enterem, potem od razu skrót → nagrywanie startuje (klawisze puszczone w trakcie przechwytywania nie blokują skrótu). Kliknięcie „Zmień” w trakcie nagrywania anuluje to nagranie.

## Mikrofon i wskaźnik

- [ ] Wskaźnik pojawia się na dole ekranu, na którym jest aktywne okno, nie zabiera fokusu (kursor tekstowy zostaje w Notatniku), słupki reagują na głos.
- [ ] Po puszczeniu klawisza wskaźnik pokazuje „Transkrypcja…”, potem „Porządkowanie…”, potem „Wklejono”.
- [ ] Kliknięcie we wskaźnik podczas nagrywania kończy dyktowanie jak drugie naciśnięcie skrótu, we wszystkich trybach skrótu (także przy wciąż trzymanym skrócie; puszczenie go potem nic nie robi). Kursor nad wskaźnikiem zmienia się w rączkę, a wskaźnik lekko się rozjaśnia. Tekst wkleja się do okna, w którym zaczęto dyktować, a kursor tekstowy zostaje w Notatniku. W stanach „Transkrypcja…”, „Wklejono” i błędu kliknięcia przechodzą do okna pod wskaźnikiem.
- [ ] Podczas nagrywania nazwa trybu na wskaźniku jest przyciskiem z ⇄ (cała nazwa widoczna, bez „…”). Kliknięcie zmienia nazwę na „Surowa transkrypcja” i nie kończy nagrywania, a kursor tekstowy zostaje w Notatniku. Po puszczeniu skrótu wkleja się surowy tekst (bez „Porządkowanie…”), w menu zasobnika i na stronie „Tryby” zaznaczony jest nowy tryb, a kolejne dyktowanie zaczyna się w nim. Drugie kliknięcie wraca do „Prompt dla agenta”. Przy jednym trybie nazwa jest zwykłym tekstem.
- [ ] Po zniknięciu wskaźnika miejsce na dole ekranu nie przechwytuje kliknięć (np. przycisk paska zadań albo okna pod spodem działa normalnie), a kolejne dyktowanie pokazuje wskaźnik od razu.
- [ ] Animacje: wskaźnik wjeżdża od dołu, słupki falują od środka w rytm głosu, przy przetwarzaniu fala i obwódka są fioletowe, po wklejeniu kropka zmienia się w zielony ✓ i wskaźnik zanika, przy błędzie wskaźnik drży. Kolejne dyktowanie nie pokazuje nawet na chwilę poprzedniego stanu. Przy wyłączonych „Efektach animacji” w Windows (Ułatwienia dostępu → Efekty wizualne) wskaźnik tylko się pojawia i znika, bez ruchu.
- [ ] Pierwsze słowo wypowiedziane razem z naciśnięciem nie jest ucięte (bufor 300 ms przy „Mikrofon gotowy w tle”).
- [ ] Dźwięki: po naciśnięciu skrótu słychać jedno krótkie piknięcie, po puszczeniu jedno niższe (bez ciągłego tonu), także na głośnikach laptopa przy zwykłej głośności i przy włączonym „Mikrofon gotowy w tle”. Escape w trakcie nagrania i pojedyncze krótkie stuknięcie nie dają sygnału końca. Przy skrócie `RightCtrl` kombinacja `RightCtrl+C` nie piszczy. Sygnał nie trafia do transkrypcji. Odznaczenie „Piknięcia” (strona „Dźwięk i mikrofon”) i „Zapisz” wycisza oba sygnały bez restartu.
- [ ] Wybór innego mikrofonu w ustawieniach działa bez restartu; odłączenie mikrofonu USB w trakcie nagrania anuluje dyktowanie z komunikatem „Mikrofon przestał działać” (nic się nie wkleja), po podłączeniu nagrywanie wraca.
- [ ] Z wyłączonym „Mikrofon gotowy w tle” ikona mikrofonu w zasobniku Windows znika między dyktowaniami, a nagrywanie nadal działa.
- [ ] Strona „Rozpoznawanie mowy” pod „Dostawca” pokazuje używany model („Teraz: scribe_v2_realtime, na żywo.”); po wyłączeniu przełącznika „Na żywo (realtime)” od razu „scribe_v2, wsadowo”. Kafelek „Mowa → tekst” na przeglądzie pokazuje to samo, a kolumna „Na żywo” w czasach ostatnich dyktowań ma „tak”.

## Wklejanie i schowek

- [ ] Wklejanie działa w: Notatniku, Windows Terminal (PowerShell), VS Code, Chrome (pole tekstowe), Claude/ChatGPT desktop.
- [ ] Skopiuj obrazek → podyktuj → po ~1 s w schowku znów jest obrazek (przywrócenie schowka). Dwa dyktowania tuż po sobie → po obu w schowku jest nadal obrazek, nie tekst pierwszego dyktowania.
- [ ] Dyktowany tekst nie pojawia się w historii schowka Win+V.
- [ ] „Wklejaj do aktywnego okna” wyłączone → tekst tylko w schowku, bez Ctrl+V; wskaźnik pokazuje „Skopiowano do schowka”.
- [ ] Przytrzymaj Shift dłużej niż sekundę po zakończeniu dyktowania → zamiast Ctrl+Shift+V komunikat „puść klawisze…”, tekst zostaje w schowku.
- [ ] Przełączenie okna w trakcie przetwarzania → tekst trafia do okna, w którym jesteś teraz (nie zabieramy fokusu celowo przełączonej aplikacji).

## Tryby i post-processing

- [ ] Tryb „Prompt dla agenta”: podyktuj z wypełniaczami i samopoprawką („najpierw testy, to znaczy nie, najpierw linter”) → wynik bez wypełniaczy, tylko wersja ostateczna, nic nie dopisane. Twoje słowa i kolejność zostają (bez zamiany na synonimy i styl urzędowy), pytanie „czy możesz…” zostaje pytaniem.
- [ ] Przełączenie trybu z menu zasobnika (Tryb → Surowa transkrypcja) → wkleja surową transkrypcję, szybciej. Uwaga: tryb „Surowa transkrypcja” zapisany wcześniej z włączonym post-processingiem pokazuje się jako „Porządkowanie tekstu”; dla surowego tekstu wybierz „Bez AI”.
- [ ] Strona „Tryby”: kliknięcie kółka przy trybie i „Zapisz” zmienia aktywny tryb (znaczek w menu zasobnika i kafelek „Tryb”). Rodzaj „Bez AI” chowa model i prompt, a dyktowanie wkleja surowy tekst bez wskaźnika „Porządkowanie…”.
- [ ] Tryb z własnym modelem (np. „Model AI” → `claude-sonnet-5-5`): dyktowanie w tym trybie ma w `timings.jsonl` i w historii `llm` z tym modelem, a inne tryby nadal model ze strony „Post-processing”. „Domyślny (…)” pokazuje aktualny model globalny.
- [ ] „Dodaj tryb” tworzy „Tryb N” z rodzajem „Porządkowanie tekstu” i od razu zaznaczoną nazwą do edycji; po „Zapisz” jest w menu zasobnika. Usunięcie aktywnego trybu przełącza na pierwszy.
- [ ] Wyłączony CLIProxy / zły token → wkleja się surowa transkrypcja, wskaźnik pokazuje błąd post-processingu.
- [ ] Backend `claude -p` / `codex exec` (jeśli CLI są w PATH) działa bez okna konsoli; „Model AI” trybu jest wtedy zwykłym polem tekstowym.
- [ ] Po dyktowaniu w `%LOCALAPPDATA%\HyperRust\logs\history.jsonl` jest wpis z surową transkrypcją (`transcript`) i wklejonym tekstem (`output`), a przy ustawionym serwerze historii ten sam wpis jest na serwerze w `~/hyperrust-history/<nazwa komputera>.jsonl` po ok. 3 s bez dyktowania (przy kilku dyktowaniach pod rząd wszystkie dochodzą razem po ostatnim). Przy wyłączonym Tailscale dyktowanie działa normalnie, a brakujące wpisy dochodzą na serwer razem z następnym dyktowaniem. W wydaniu z GitHuba pole „Serwer historii” jest puste i nic nie jest wysyłane.
- [ ] Strona „Historia” pokazuje te same dyktowania od najnowszych, pogrupowane na „Dzisiaj”, „Wczoraj” i daty, z nazwą trybu i czasem do wklejenia. Wpis z błędem AI ma czerwoną plakietkę. Wyszukiwarka filtruje po wklejonym tekście i transkrypcji. „Kopiuj” wkłada wklejony tekst do schowka (Ctrl+V w Notatniku), „Kopiuj transkrypcję” surowy tekst. Po nowym dyktowaniu i ponownym wejściu na stronę wpis jest na górze. Przy wyłączonym „Zapisuj historię dyktowań” strona mówi, że zapis jest wyłączony.

## Autoaktualizacja

- [ ] Wydanie z GitHuba: po opublikowaniu nowszego wydania (workflow „Wydanie”) aplikacja pobiera je przez przekierowania GitHuba i restartuje się; build z `pnpm tauri build` bez `HYPERRUST_BUILD` pokazuje w „Ogólne”, że nie aktualizuje się sam.
- [ ] Na własnym serwerze: `scripts/publish-dev.sh` → w ciągu ~30 s (gdy nic nie dyktujesz) aplikacja sama się restartuje; w ustawieniach jest nowy numer buildu, w folderze EXE po ~30 s nie zostaje `*.old`, `*.update.exe` ani `*.update-ok`.
- [ ] W trakcie dyktowania aktualizacja czeka, aż skończysz; skrót naciśnięty w chwili podmiany pokazuje „Trwa instalacja aktualizacji”.
- [ ] Wycofanie: opublikuj build, który kończy się przy starcie (np. tymczasowy `panic!` w `setup`) → po chwili wraca poprzednia wersja, obok EXE jest `*.failed.exe`, a status aktualizacji mówi, że build został wycofany i nie jest pobierany ponownie.
- [ ] Wyłączenie aktualizacji w ustawieniach, gdy status to „gotowa”, wstrzymuje instalację.
- [ ] „Sprawdź aktualizacje” w zasobniku działa natychmiast; przy wyłączonym Tailscale status pokazuje błąd serwera, aplikacja działa dalej.
- [ ] Autostart włączony → po aktualizacji nadal startuje z Windows (ta sama ścieżka EXE).

## Wydajność

- [ ] Z logu `timings.jsonl`: `release_to_stt_ms` ≈ 0,2–0,4 s dla ElevenLabs realtime, `release_to_paste_ms` w trybie „Prompt dla agenta” ≈ 2–3,5 s (przy minimalnej redakcji Haiku zwykle chwilę myśli; z `reasoning_effort: none` ≈ 1,3–1,8 s).
- [ ] Start aplikacji do gotowości skrótu < 1 s; pamięć w Menedżerze zadań (bez otwartych ustawień) ok. 30–60 MB.
