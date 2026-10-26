import openai
import ragas


def score() -> None:
    ragas.evaluate(openai.OpenAI())
