from chat import testing


def create():
    import openai

    return openai.OpenAI(), testing
