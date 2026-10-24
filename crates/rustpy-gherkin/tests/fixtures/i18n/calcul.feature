# language: fr
Fonctionnalité: Calcul
  Contexte:
    Soit une calculatrice

  Scénario: Addition
    Quand j'additionne 2 et 3
    Alors le résultat est 5
    Et qu'il n'y a pas d'erreur

  Plan du scénario: Addition de <a> et <b>
    Lorsqu'on additionne <a> et <b>
    Alors le résultat est <somme>

    Exemples:
      | a | b | somme |
      | 1 | 1 | 2     |
