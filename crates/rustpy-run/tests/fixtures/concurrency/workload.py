"""Manual/final-stage capture fixture: lock contention, tasks, cancellation.

Do not execute during source preparation. Expected: named worker thread,
producer/consumer task lanes, matching lock resource and source locations,
await suspensions, target stdout preserved, and cancelled task span closure.
"""
import asyncio
import threading
import time

lock = threading.Lock()
ready = threading.Event()


def worker():
    ready.set()
    with lock:
        print("worker acquired lock")


async def consumer(gate):
    async with gate:
        await asyncio.sleep(0.02)


async def produce():
    gate = asyncio.Lock()
    first = asyncio.create_task(consumer(gate), name="producer")
    second = asyncio.create_task(consumer(gate), name="consumer")
    cancelled = asyncio.create_task(asyncio.sleep(10), name="cancelled")
    await asyncio.gather(first, second)
    cancelled.cancel()
    try:
        await cancelled
    except asyncio.CancelledError:
        print("task cancelled")


if __name__ == "__main__":
    with lock:
        thread = threading.Thread(target=worker, name="blocked-worker")
        thread.start()
        ready.wait()
        time.sleep(0.02)
    asyncio.run(produce())
    # Deliberately leave the worker join to normal interpreter shutdown.
