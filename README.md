# Disk Copy 98

Disk Copy 98 è un’app portatile per copiare, creare, spostare ed eliminare file e cartelle su Windows. Funziona con unità locali, chiavette USB e percorsi di rete. Tramite WSL 2 può aprire anche volumi Linux che Windows mostra senza lettera o come **RAW**, tra cui alcuni dischi di NAS ReadyNAS.

## Scarica e avvia

1. Apri [**DiskCopy98.exe**](./DiskCopy98.exe) su GitHub e usa **Download raw file** per scaricarlo.
2. Avvia l’eseguibile con un doppio clic su **Windows 10 o 11 a 64 bit (x64)**.
3. Per le unità già leggibili in Windows basta l’eseguibile. Per un disco Linux o RAW, completa la sezione [Installa WSL 2](#installa-wsl-2).

## Copia e sposta file

1. Scegli l’unità di **Origine** nel pannello a sinistra e quella di **Destinazione** nel pannello a destra. Puoi anche digitare o incollare un percorso completo e premere **Sfoglia** nel relativo pannello. **Auto disco** dà priorità alle unità USB e rimovibili.
2. A destra apri la cartella in cui vuoi trasferire i file. Entra con il doppio clic o con **Apri** e usa **Su** per risalire. Il percorso sopra l’elenco indica la cartella di arrivo. La cartella deve essere scrivibile e trovarsi fuori dall’origine.
3. A sinistra spunta i file o le cartelle da trasferire. Le spunte restano attive mentre navighi e la spunta su una cartella include il suo contenuto. Sotto l’elenco compare la dimensione totale degli elementi spuntati in **MB** o **GB**.
4. Scegli l’operazione.

   | Comando | Risultato |
   | --- | --- |
   | **Copia selezione** | Copia gli elementi spuntati e lascia gli originali al loro posto. |
   | **Copia tutto** | Copia l’intera origine quando la selezione è vuota. Usa **Azzera spunte** per tornare a questa modalità. |
   | **Sposta selezione** | Chiede conferma. Rimuove ciascun file dall’origine dopo averlo scritto nella destinazione. |

5. Attendi il riepilogo. La barra mostra la percentuale elaborata e il testo sotto la barra indica quanti byte sono stati scritti. Il pannello a destra si aggiorna dopo il trasferimento.

L’app conserva i percorsi relativi. Se in `E:\Foto` selezioni `2025\Vacanze\mare.jpg`, nella destinazione troverai la stessa sottocartella `2025\Vacanze`. **Help** o **F1** aprono la guida nell’app.

### Nomi già presenti e file saltati

Il riepilogo indica il motivo e il percorso degli elementi saltati. Un risultato come **0 file spostati, 1 saltato** può comparire se in destinazione esiste già un elemento con lo stesso nome. Spunta **Mantieni entrambi se il nome esiste già** e riprova per salvare il file in arrivo come `mare (1).jpg`, conservando quello esistente. In alternativa, scegli una cartella di destinazione diversa.

I collegamenti vengono saltati. Se un errore interrompe il trasferimento, gli elementi già trasferiti restano in destinazione. Se la copia di un file riesce ma la rimozione fallisce, l’app segnala l’errore e il file resta anche all’origine.

## Gestisci file e cartelle

Puoi usare i comandi di gestione in entrambi i pannelli, nella cartella indicata sopra l’elenco.

| Comando | Come funziona |
| --- | --- |
| **Nuovo file…** o **Nuova cartella…** | Inserisci un nome e premi **Crea** per aggiungere un file vuoto o una cartella vuota. |
| **Sposta…** | Evidenzia una riga, indica una cartella esistente e premi **Sposta**. Puoi scegliere una cartella dello stesso disco o di un’altra unità. Se è aperta una cartella nell’altro pannello, l’app ne propone il percorso. |
| **Elimina…** | Evidenzia una riga e conferma dopo aver verificato il percorso. L’eliminazione salta il Cestino. Quando elimini una cartella, rimuovi anche tutto il suo contenuto. |

**Sposta…** conserva il nome dell’elemento e si ferma se quel nome è già presente nella cartella di arrivo. Per trasferire più elementi insieme, usa le spunte a sinistra e **Sposta selezione**. I pannelli si aggiornano dopo ogni operazione.

## Installa WSL 2

WSL permette a Disk Copy 98 di usare Linux per leggere un disco. Per la procedura qui sotto serve **Windows 11** oppure **Windows 10 versione 2004, build 19041, o successiva**. Per i dischi USB usa una versione aggiornata di WSL, anche su Windows 10. [Guida Microsoft all’installazione di WSL](https://learn.microsoft.com/it-it/windows/wsl/install)

### Prima installazione

1. Apri **Start**, cerca **PowerShell** e scegli **Esegui come amministratore**.
2. Installa WSL con Ubuntu.

   ```powershell
   wsl --install -d Ubuntu
   ```

3. Riavvia Windows. Apri **Ubuntu** dal menu Start, attendi l’inizializzazione e crea utente e password Linux quando richiesto.
4. In PowerShell esegui questi comandi. Nella colonna `VERSION` di Ubuntu deve comparire `2`.

   ```powershell
   wsl --update
   wsl --list --verbose
   ```

Se il download di Ubuntu resta fermo, prova `wsl --install --web-download -d Ubuntu` da PowerShell come amministratore. Se Windows segnala che la macchina virtuale non può avviarsi, verifica **Piattaforma macchina virtuale** nelle funzionalità di Windows e la virtualizzazione nel firmware del PC. [Risoluzione dei problemi di WSL](https://learn.microsoft.com/it-it/windows/wsl/troubleshooting)

### Se WSL è già installato

Controlla le distribuzioni con `wsl --list --verbose`. Puoi usare una distribuzione WSL 2 già inizializzata, come Ubuntu o Debian. Se vedi soltanto `docker-desktop`, installa Ubuntu seguendo la procedura sopra. Se Ubuntu è presente con versione `1`, esegui `wsl --set-version Ubuntu 2`. Per un’altra distribuzione, usa il nome esatto mostrato nell’elenco. [Comandi di base di WSL](https://learn.microsoft.com/it-it/windows/wsl/basic-commands)

### Collega i dischi USB a WSL

Per passare un disco USB a WSL, Disk Copy 98 usa **usbipd-win 5 o successivo**. La prima volta prova a installarlo con WinGet se manca. Windows può chiederti di autorizzare l’installazione e, separatamente, la condivisione del disco selezionato. [Guida Microsoft al collegamento USB](https://learn.microsoft.com/it-it/windows/wsl/connect-usb)

Per installarlo manualmente, apri PowerShell ed esegui questo comando. Se WinGet è assente, scarica l’installer `.msi` per x64 dalle [release ufficiali di usbipd-win](https://github.com/dorssel/usbipd-win/releases).

```powershell
winget install --interactive --exact --id dorssel.usbipd-win
```

## Usa un disco Linux o RAW

1. Collega il disco. Se Windows propone di inizializzarlo o formattarlo e vuoi conservare i dati, annulla la richiesta.
2. Nell’app scegli il disco fisico in **Origine**. Se **Auto disco** lo ha già selezionato, premi **Sfoglia** a sinistra. Per scrivere su un volume Linux compatibile, puoi scegliere il disco in **Destinazione** e aprire la cartella di arrivo nel pannello a destra.
3. Attendi il collegamento a WSL. Un disco USB viene assegnato temporaneamente a Linux tramite usbipd-win. Per gli altri dischi supportati l’app usa il montaggio di WSL e Windows può chiedere l’autorizzazione amministrativa. Se il disco espone più volumi, scegli quello desiderato dal menu. [Montaggio dei dischi in WSL 2](https://learn.microsoft.com/it-it/windows/wsl/wsl2-mount-disk)
4. Per trasferire file dal disco a Windows, indica una cartella scrivibile su **C:**, seleziona gli elementi e scegli **Copia selezione** o **Sposta selezione**. Le opzioni dipendono dalla possibilità di scrivere sul volume.
5. Al termine premi **Espelli WSL** e attendi la conferma dello smontaggio prima di scollegare il cavo. Alla chiusura l’app tenta lo smontaggio e si chiude anche in caso di errore.

### Dischi da NAS ReadyNAS SPARC

L’app riconosce alcuni layout ReadyNAS legacy, come quello verificato sul **Samsung HD103UJ**. Può aprire un volume dati ext su un disco con un singolo volume LVM lineare. La copia parte in sola lettura. Per il layout ReadyNAS SPARC supportato, quando confermi un’operazione di gestione o uno spostamento, l’app controlla il filesystem, apre il volume in scrittura e ripristina la sola lettura al termine. Se il filesystem richiede un controllo oppure il volume dipende da più dischi, può essere necessario usare il NAS originale o uno strumento di recupero.

Su Ubuntu o Debian l’app prova a installare `lvm2` e `fuse2fs` al bisogno. Se l’installazione automatica fallisce, puoi installarli dal terminale di Ubuntu e riprovare.

```bash
sudo apt update
sudo apt install lvm2 fuse2fs
```

### Percorsi WSL già montati

Puoi anche incollare direttamente un percorso come `\\wsl.localhost\Ubuntu\home\utente\Foto`. Usa il nome esatto della distribuzione mostrato da `wsl --list --verbose`. L’accesso segue i permessi dell’utente Linux. I trasferimenti conservano il contenuto dei file e la struttura delle cartelle. Permessi e proprietà Linux e altri metadati richiedono strumenti di backup dedicati.

## Risoluzione dei problemi

| Problema | Cosa controllare |
| --- | --- |
| Il disco USB non compare | Ricontrolla cavo, alimentazione e adattatore. In PowerShell, `Get-Disk` mostra i dischi rilevati da Windows e `usbipd list` mostra i dispositivi USB. Premi di nuovo **Auto disco** nell’app. |
| WSL vede l’adattatore ma il disco non si apre | Chiudi Disk Copy 98 e le altre app che usano WSL. Esegui `wsl --shutdown` e `wsl --update`, riapri Ubuntu e riprova. Il kernel WSL deve supportare l’archiviazione USB. |
| Windows indica **RAW** e anche l’app non trova un filesystem | **RAW** è lo stato mostrato da Windows. Il disco può contenere un filesystem Linux, un volume cifrato, dati di un RAID o una struttura danneggiata. Usa il messaggio dell’app per capire se WSL ha rilevato un volume. |
| **Sposta selezione** è disattivato | Spunta almeno un elemento. Se il volume è disponibile solo in lettura, usa **Copia selezione**. |
| Errore di accesso alla destinazione su C: | Usa una cartella in cui puoi scrivere, per esempio una sottocartella di **Documenti**, e verifica che ci sia abbastanza spazio. |
| Il disco risulta ancora collegato a WSL | Attendi il termine del trasferimento, chiudi eventuali finestre che usano il volume e riprova con **Espelli WSL**. |

## Ricompila dai sorgenti

Installa [Rustup](https://rustup.rs/), apri PowerShell nella cartella del repository ed esegui questi comandi.

```powershell
rustup toolchain install stable-x86_64-pc-windows-gnu
cargo +stable-x86_64-pc-windows-gnu test --release --locked --target x86_64-pc-windows-gnu
powershell -NoProfile -File .\scripts\package.ps1
```

Lo script ricrea `DiskCopy98.exe` e incorpora l’icona nella versione portatile.
