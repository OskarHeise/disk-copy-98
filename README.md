# Disk Copy 98

Disk Copy 98 copia e sposta file e cartelle tra il PC, chiavette USB e altri dischi accessibili da Windows.

## Scarica e avvia

Scarica [**DiskCopy98.exe**](DiskCopy98.exe) e aprilo con un doppio clic. È un singolo file portatile per **Windows 10 e 11 a 64 bit (x64)**. Non servono installazione, Java, Rust o DLL aggiuntive. Per usarlo su un altro PC, trasferisci solo `DiskCopy98.exe`.

## Prima copia di prova

1. In Esplora file, crea due cartelle separate, ad esempio `ProvaOrigine` e `ProvaDestinazione` dentro **Documenti**. Metti un paio di file di prova nella cartella di origine.
2. Apri Disk Copy 98. Inserisci i percorsi completi nei campi **Origine** e **Destinazione**. Per lavorare con una chiavetta puoi scegliere la sua unità dal menu oppure digitare un percorso come `E:\Foto`. **Auto disco** dà priorità alle unità rimovibili.
3. Premi **Mostra file**. Metti la spunta accanto a un file e premi **Copia selezione**. Per entrare in una sottocartella, usa il doppio clic o **Apri**. Con **Su** risali di un livello.
4. Attendi il riepilogo in basso e apri la destinazione in Esplora file. Il file copiato dovrebbe comparire anche lì. Se hai scelto un file in una sottocartella, l'app ricrea il suo percorso relativo.

Il menu **Origine** mostra anche i dischi senza lettera di unità. **Auto disco** segnala un disco USB senza lettera prima di scegliere l'unità di sistema. Se Windows indica il disco come **RAW**, i file da sfogliare richiedono un volume leggibile.

## Copia tutto o sposta una selezione

- **Copia tutto** copia l’intero contenuto dell’origine quando la selezione è vuota. Usa **Azzera spunte** per tornare a questa modalità.
- **Copia selezione** copia i file e le cartelle spuntati. Le spunte restano attive mentre navighi e selezionare una cartella include il suo contenuto.
- **Sposta selezione** richiede una conferma. L’app rimuove ogni file dall’origine dopo aver scritto la copia nella destinazione.

La destinazione deve esistere ed essere esterna alla cartella di origine. I file già presenti e i collegamenti vengono saltati. Se un errore interrompe il trasferimento, gli elementi già trasferiti restano nella destinazione. **Help** o **F1** aprono la guida dall’app.

## File Linux accessibili tramite WSL

Se usi già WSL, puoi inserire un percorso come `\\wsl.localhost\Ubuntu\home\utente\Foto`. Sostituisci `Ubuntu` con il nome mostrato da `wsl --list --quiet`. Il disco deve essere montato nella distribuzione. La copia conserva il contenuto dei file e la struttura delle cartelle. Permessi Linux e altri metadati non vengono replicati.

## Ricompila dai sorgenti

Sul PC di sviluppo con Rustup, esegui questi comandi da PowerShell nella cartella del progetto.

```powershell
rustup toolchain install stable-x86_64-pc-windows-gnu
cargo +stable-x86_64-pc-windows-gnu test --release --locked --target x86_64-pc-windows-gnu
powershell -NoProfile -File .\scripts\package.ps1
```

Lo script ricrea `DiskCopy98.exe` e incorpora l'icona nella versione portatile.
