# language: de
@rechner
Funktionalität: Rechner
  Um Fehler zu vermeiden
  möchte ich addieren.

  Grundlage:
    Angenommen ein neuer Rechner

  Szenario: Zwei Zahlen addieren
    Wenn ich 2 und 3 addiere
    Dann ist das Ergebnis 5
    Und es gibt keinen Fehler

  Szenariogrundriss: Addieren von <a> und <b>
    Wenn ich <a> und <b> addiere
    Dann ist das Ergebnis <summe>

    Beispiele:
      | a | b | summe |
      | 1 | 2 | 3     |
      | 4 | 5 | 9     |
