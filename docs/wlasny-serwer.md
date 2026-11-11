# Własny serwer: aktualizacje, historia i ranking

Wszystko tutaj jest opcjonalne. Wydania z GitHuba działają bez serwera: aktualizują się z GitHub
Releases, historię dyktowań trzymają tylko na komputerze i nie pokazują rankingu modeli.

Własny serwer to dowolna maszyna z Linuksem w twojej sieci Tailscale. Jej adresy nigdy nie trafiają
do repozytorium: skrypty czytają je z `~/.config/hyperrust/build.env` (zmienne powłoki, inny plik
wskażesz w `HYPERRUST_CONF`), a `scripts/publish-dev.sh` wpisuje domyślne adresy w budowany EXE:

| Zmienna | Do czego |
|---|---|
| `HYPERRUST_DEFAULT_UPDATE_URL` | adres `latest.json` dla nowych instalacji (bez niej skrypt odmawia budowania) |
| `HYPERRUST_DEFAULT_HISTORY_URL` | serwer historii, na który nowe instalacje wysyłają dyktowania |
| `HYPERRUST_DEFAULT_LLM_URL` | endpoint zgodny z OpenAI (np. CLIProxy), od którego startuje nowa instalacja |
| `HYPERRUST_UPDATE_BIND`, `HYPERRUST_HISTORY_BIND` | adres Tailscale, na którym słuchają serwery aktualizacji i historii |
| `HYPERRUST_RANKING_BWS_PROJECT`, `CLIPROXY_BASE_URL` | projekt w Bitwarden Secrets Manager z kluczem OpenRouter i endpoint sędziego rankingu |

## Własny serwer aktualizacji (Tailscale)

Wydania publiczne aktualizują się z GitHub Releases. Własny serwer przydaje się, gdy chcesz, żeby po każdej zmianie kodu twoje kopie same dostały nowy build:

1. Na serwerze raz: `scripts/install-update-server.sh`. Uruchamia unit użytkownika `hyperrust-updates`, czyli serwer plików `~/hyperrust-updates` na porcie 8790, tylko pod adresem z `HYPERRUST_UPDATE_BIND` (adres Tailscale serwera).
2. Po zmianach: `scripts/publish-dev.sh` (szybki profil `dev-release`) albo `scripts/publish-dev.sh --release`. Skrypt buduje EXE przez `cargo-xwin`, zapisuje `HyperRust-<build>.exe` i `latest.json` (build, czas, sha256, rozmiar, opis commita) i zostawia 5 ostatnich buildów.
3. Aplikacja co 30 s sprawdza `latest.json` (tylko build z ustawionym `HYPERRUST_BUILD`, czyli wydanie albo build ze skryptu; zwykły `cargo build` nie aktualizuje się sam). Nowszy build pobiera w tle, weryfikuje sha256, rozmiar i nagłówek PE. Gdy nic nie dyktujesz (5 s spokoju), podmienia swój EXE (rename działającego pliku na `*.exe.old`) i restartuje się. Ustawienia, klucze i autostart zostają.
4. Poprzednia wersja pilnuje startu nowej. Jeśli nowy build zakończy się albo nie wstanie w ciągu 60 s, wraca stary EXE, nieudany zostaje jako `*.failed.exe`, a jego sha256 trafia na listę pominiętych (nie jest pobierany ponownie, dopóki nie opublikujesz nowszego).

Zabezpieczenia: `http://` jest akceptowane tylko dla adresów Tailscale (100.64.0.0/10, `*.ts.net`) i lokalnych, bo WireGuard szyfruje i uwierzytelnia ruch; inne adresy muszą być `https://`, a przekierowania są wykonywane tylko z HTTPS na HTTPS (GitHub przekierowuje pobieranie dwa razy). Starszy lub ten sam build nigdy nie jest instalowany. Aktualizacje wyłączysz w ustawieniach (także gdy aktualizacja czeka już na instalację). Cofnięcie zmian: opublikuj starszy commit ponownie (dostanie nowszy znacznik czasu).

## Historia dyktowań (do strojenia promptu)

Po każdym dyktowaniu aplikacja zapisuje wpis, który pokazuje, co zwrócił model transkrypcji i co z tego zrobił post-processing:

- `transcript`: surowa transkrypcja STT;
- `output`: tekst, który został wklejony (przy błędzie post-processingu to surowa transkrypcja);
- `prompt_sha`: skrót promptu systemowego, czyli która wersja promptu dała wynik;
- `stt_model`, `llm`, `mode`, `outcome`, `build`, `device`, czasy etapów i koszt, takie jak w `timings.jsonl`.

Wpisy trafiają do `%LOCALAPPDATA%\HyperRust\logs\history.jsonl` (ostatnie 300) zaraz po wklejeniu: zapis trwa milisekundy w osobnym wątku. Strona „Historia” w ustawieniach (także „Historia…” w menu zasobnika) pokazuje je od najnowszych, pogrupowane po dniach, z wyszukiwarką i przyciskami „Kopiuj” (wklejony tekst) i „Kopiuj transkrypcję” (surowy tekst, gdy AI go zmieniło). Gdy w „Zaawansowanych” jest adres serwera historii (w wydaniach publicznych pole jest puste), ostatnie 20 jest wysyłane na ten serwer (np. `http://100.x.y.z:8791/history`, ta sama zasada adresów co przy aktualizacjach) dopiero po 3 s bez dyktowania, więc wysyłka nigdy nie dzieli sieci z dyktowaniem, a kilka dyktowań pod rząd daje jedną wysyłkę. Serwer zapisuje każdy wpis raz, więc dyktowania z czasu, gdy serwer był niedostępny, dochodzą z następnym. Do logu nigdy nie trafia dyktowany tekst. Historię wyłączysz na stronie „Ogólne”, a wysyłkę pod „Zaawansowane” (puste pole „Serwer historii”).

Na serwerze:

1. Serwer raz instaluje `scripts/install-history-server.sh`. Tworzy unit użytkownika `hyperrust-history` na adresie Tailscale, który zapisuje do `~/hyperrust-history/<nazwa komputera>.jsonl` (ostatnie 2000 wpisów, pliki 0600). Serwer tylko przyjmuje wpisy i nie udostępnia ich przez sieć.
2. Przegląd: `jq -r 'select(.llm != "") | "\(.outcome) \(.prompt_sha)\n> \(.transcript)\n= \(.output)\n"' ~/hyperrust-history/*.jsonl | tail -60`.
3. Nowy prompt na prawdziwych dyktowaniach: `CLIPROXY_API_KEY=… cargo run -p hyperrust-core --example prompt_eval -- --history ~/hyperrust-history/<komputer>.jsonl --last 20 --prompt-file nowy.txt`. Dla każdego dyktowania przykład wypisuje surową transkrypcję, wynik z aplikacji i wynik nowego promptu.

## Ranking modeli post-processingu

W buildzie z własnym serwerem aktualizacji strona „Post-processing” pokazuje ranking modeli, który ten serwer liczy codziennie (wydania z GitHuba tej tabeli nie mają). Aplikacja tylko go pobiera: `ranking.json` leży obok `latest.json` na serwerze aktualizacji, pod adresem z ustawień aktualizacji. Zewnętrzne rankingi (np. Artificial Analysis) mierzą ogólną inteligencję i średnią szybkość wszystkich dostawców, a nie redakcję polskiego dyktowania przez szybką trasę OpenRouter, więc serwer mierzy to sam.

- **Zestaw i zapytanie.** Każdy model dostaje te same 24 dyktowania: 14 z `tests/fixtures/dictations.txt` i 10 prawdziwych z historii dyktowań (`~/hyperrust-ranking/set/hist.jsonl` na serwerze, poza repo). Zapytanie jest takie jak w aplikacji: wbudowany prompt agenta, OpenRouter z dopiskiem `:nitro` i reasoning effort `none`. Modele, które nie pozwalają wyłączyć myślenia (HTTP 400 „Reasoning is mandatory”), dostają `low`.
- **Ocena i czas.** Ślepy sędzia (Opus 5.5 przez CLIProxy, w abonamencie) daje każdemu wynikowi 0–10 za wierność, oczyszczenie, polszczyznę i terminy, nie wiedząc, który model go napisał. Obok nowych modeli widzi zapisane wyniki 3 najlepszych, więc skala zostaje ta sama między przebiegami. Nieudane zapytanie liczy się jako 0. Czas to mediana od wysłania zapytania do pełnej odpowiedzi; koszt to mediana kwoty, którą policzył OpenRouter.
- **Kolejność.** Liczą się szybkość i koszt, a ocena jest progiem. Najpierw idą modele z oceną co najmniej 8, ustawione według iloczynu mediany czasu i kosztu, więc 2× szybciej waży tyle co 2× taniej. Pod nimi jest reszta według oceny, a na końcu modele bez oceny („przerwany”, „niedostępny”). Ta kolejność wyznacza „najlepszych” w regułach poniżej. Sędzia jako odniesienie dostaje wyniki 3 modeli z najwyższą oceną, nie z czołówki tej kolejności.
- **Co i kiedy.** Timer `hyperrust-ranking.timer` uruchamia przebieg codziennie o 5:40 (przegapiony nadrabia po starcie serwera). Pełny test model dostaje raz: każdy model czatu z katalogu OpenRouter dodany w ostatnich 45 dniach, do 5 $ za 1M tokenów odpowiedzi. Wyjątek to model przetestowany w pierwszych 2 dniach od premiery, gdy dostawcy bywają przeciążeni: drugi pełny test dostaje w pierwszym przebiegu po ukończeniu 2 dni. Ranking pokazuje wtedy wynik drugiego testu, a czas z pierwszego zostaje w historii pomiarów. Test przerywa się po 6 dyktowaniach, gdy mediana przekracza 8 s; taki model zostaje w rankingu jako „przerwany” i poza tym wyjątkiem nie jest testowany ponownie. Model, którego dostawca odmawia (HTTP 400/404/422, np. 404, gdy wymaga zgody na trenowanie na danych w ustawieniach konta OpenRouter), trafia do rankingu jako „niedostępny” z powodem. Test z samymi błędami innego rodzaju (sieć, klucz, brak środków, limit zapytań) nie trafia do dziennika, więc następny przebieg go powtarza. Najlepszych 5 jest mierzonych ponownie co 7 dni (sam czas, bo szybkość zmienia się z dostawcami i obciążeniem, a ocena nie). Po zmianie promptu agenta najlepszych 6 przechodzi pełny test od nowa, a reszta ma plakietkę „inny prompt”. Starszy model testujesz ręcznie: `--models dostawca/model`.
- **Historia.** Każdy test to wiersz w `~/hyperrust-ranking/measurements.jsonl`: data, model, effort, wersje promptu i zestawu, ocena, liczba dyktowań z oceną 9–10, mediana i 90. percentyl czasu, koszt, błędy i uwagi sędziego. Wyniki modeli są w `~/hyperrust-ranking/outputs/`. Ranking jest wyliczany z tego dziennika, a aplikacja po najechaniu na czas pokazuje wcześniejsze pomiary.
- **Czy działa.** Notka pod tabelą podaje datę rankingu. Gdy ranking ma ponad 2 dni, aplikacja mówi, że zadanie rankingu nie działa. Log: `journalctl --user -u hyperrust-ranking`.
- **Koszt.** Pełny test to 24 zapytania przez OpenRouter (klucz z Bitwarden Secrets Manager), zwykle poniżej 0,01 $ za model, a droższe modele do ok. 0,06 $.

Na serwerze raz: `scripts/install-ranking.sh`. Skrypt robi osobny checkout w `~/hyperrust-ranking/src`, który przed każdym przebiegiem przechodzi na `origin/main` (testowany jest prompt z main), i włącza timer. Przebieg od ręki: `systemctl --user start hyperrust-ranking`. Sam plan, bez testów: `OPENROUTER_API_KEY=… CLIPROXY_API_KEY=… CLIPROXY_BASE_URL=… cargo run -p hyperrust-core --example ranking -- --plan`.
