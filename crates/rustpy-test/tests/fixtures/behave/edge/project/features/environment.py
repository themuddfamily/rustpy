def before_scenario(context, scenario):
    if scenario.name == "Hook explodes":
        raise RuntimeError("hook boom")
