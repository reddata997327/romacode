"""Compose layer tests — pipeline, parallel, branch over the builtin catalog."""
import asyncio

from rc_python import Lifecycle, BuiltinSource
from rc_python.compose import (
    BranchArm,
    BranchTool,
    ParallelStep,
    ParallelTool,
    PipelineStep,
    PipelineTool,
)


def _life():
    life = Lifecycle.permissive()
    life.add_source(BuiltinSource())
    asyncio.run(life.discover())
    return life


def test_pipeline_chains_add_then_sqrt():
    life = _life()
    pipe = PipelineTool(
        name="compose.pipe.sqrt_of_sum",
        description="add a+b then sqrt the sum",
        steps=[
            PipelineStep(tool="math.add"),                                  # {a,b} -> number
            PipelineStep(tool="math.sqrt", template={"n": "{{prev}}"}),     # prev -> sqrt
        ],
        executor=life.executor,
    )
    life.register(pipe)
    out = asyncio.run(life.execute("compose.pipe.sqrt_of_sum", {"a": 7, "b": 18}, True))
    # 7 + 18 = 25, sqrt(25) = 5.0
    assert out == 5.0


def test_parallel_fans_out():
    life = _life()
    par = ParallelTool(
        name="compose.par.sum_and_root",
        description="add a+b and sqrt n in parallel",
        steps=[
            ParallelStep(name="sum", tool="math.add", template={"a": "{{input.a}}", "b": "{{input.b}}"}),
            ParallelStep(name="root", tool="math.sqrt", template={"n": "{{input.n}}"}),
        ],
        executor=life.executor,
    )
    life.register(par)
    out = asyncio.run(life.execute("compose.par.sum_and_root", {"a": 2, "b": 3, "n": 9}, True))
    assert out == {"sum": 5.0, "root": 3.0}


def test_branch_first_match_wins_and_falls_through():
    life = _life()
    br = BranchTool(
        name="compose.branch.pick",
        description="sqrt when use_sqrt else add",
        arms=[
            BranchArm(tool="math.sqrt", when="{{input.use_sqrt}}", template={"n": "{{input.n}}"}),
            BranchArm(tool="math.add", when=None, template={"a": "{{input.a}}", "b": "{{input.b}}"}),
        ],
        executor=life.executor,
    )
    life.register(br)
    # first arm matches (truthy) -> sqrt(9) = 3.0
    out = asyncio.run(life.execute("compose.branch.pick", {"use_sqrt": "yes", "n": 9, "a": 1, "b": 1}, True))
    assert out == 3.0
    # first arm empty -> catch-all -> add(2,3) = 5.0
    out2 = asyncio.run(life.execute("compose.branch.pick", {"use_sqrt": "", "n": 0, "a": 2, "b": 3}, True))
    assert out2 == 5.0
