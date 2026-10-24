from behave import given, then, when


@given("ein neuer Rechner")
def step_neu(context):
    context.ergebnis = 0


@when("ich {a:d} und {b:d} addiere")
def step_add(context, a, b):
    context.ergebnis = a + b


@then("ist das Ergebnis {e:d}")
def step_ergebnis(context, e):
    assert context.ergebnis == e


@then("es gibt keinen Fehler")
def step_kein_fehler(context):
    pass


@given("une calculatrice")
def step_calculatrice(context):
    context.resultat = 0


@when("j'additionne {a:d} et {b:d}")
@when("on additionne {a:d} et {b:d}")
def step_additionne(context, a, b):
    context.resultat = a + b


@then("le résultat est {r:d}")
def step_resultat(context, r):
    assert context.resultat == r


@then("il n'y a pas d'erreur")
def step_pas_erreur(context):
    pass
