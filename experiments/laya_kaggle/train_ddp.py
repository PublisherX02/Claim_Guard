"""2xT4 DDP fine-tune of Laya on ClaimGuard rule-check cases.

Adapted from the official notebook (convaiinnovations/laya, notebooks/laya_finetune_typed_decisions_2xT4_kaggle.ipynb).
Only the data, the paths and the schedule differ; the objective (RLCD), calibration and checkpointing are Laya's own
primitives (`laya.train`, `laya.common`, `laya.calibrate`).

    torchrun --standalone --nproc_per_node=2 train_ddp.py MODEL_DIR OUTPUT_DIR TRAIN_ITEMS_PT
"""
import json
import math
import os
import random
import sys
import time

import torch
import torch.distributed as dist
from safetensors.torch import load_file
from torch.nn.parallel import DistributedDataParallel as DDP
from transformers import AutoTokenizer

from laya.calibrate import fit_temperature_map
from laya.common import build_model, collate_items
from laya.train import calibration_records, encode_item, rlcd_loss, save_checkpoint, sigma_at, split_calibration

EPOCHS = int(os.environ.get("EPOCHS", "6"))
MICRO_BATCH = int(os.environ.get("MICRO_BATCH", "8"))   # sequences per forward pass per GPU
GRAD_ACCUM = int(os.environ.get("GRAD_ACCUM", "2"))     # effective batch = MICRO_BATCH * 2 GPUs * GRAD_ACCUM = 32
GROUP_SIZE = 4                                          # RLCD baseline samples
LR_ENCODER = float(os.environ.get("LR_ENCODER", "2.5e-5"))
LR_HEAD = float(os.environ.get("LR_HEAD", "1.0e-4"))
SIGMA_START, SIGMA_END = 0.4, 0.1                       # exploration noise


def main():
    dist.init_process_group("nccl")
    rank, world_size = dist.get_rank(), dist.get_world_size()
    local_rank = int(os.environ.get("LOCAL_RANK", "0"))
    torch.cuda.set_device(local_rank)
    device = torch.device("cuda", local_rank)

    model_dir, output_dir, items_path = sys.argv[1], sys.argv[2], sys.argv[3]

    with open(os.path.join(model_dir, "rl_agent_config.json")) as f:
        cfg = json.load(f)
    cfg["gradient_checkpointing"] = True
    cfg["max_tokens_per_batch"] = 4096
    cfg["max_len"] = 1024        # a claim plus its rule is ~600-900 tokens
    cfg["head_max_len"] = 256
    max_len, head_max_len = cfg["max_len"], cfg["head_max_len"]

    tok = AutoTokenizer.from_pretrained(os.path.join(model_dir, "tokenizer"))
    model = build_model(cfg, encoder_dir=os.path.join(model_dir, "encoder"))
    model.load_state_dict(load_file(os.path.join(model_dir, "model.safetensors")), strict=True)
    model.encoder.gradient_checkpointing_enable(gradient_checkpointing_kwargs={"use_reentrant": False})
    model.head_checkpointing = True
    model.to(device)
    model.train()
    ddp_model = DDP(model, device_ids=[local_rank], find_unused_parameters=True)

    all_items = torch.load(items_path, weights_only=False)
    # Calibration items are withheld before sharding (seed-deterministic, so every rank withholds the same ones).
    train_items, calib_items = split_calibration(all_items, calib_max=400, calib_frac=0.1, seed=20261015)
    # Every rank must run the same number of micro-batches or DDP deadlocks on the unmatched all-reduce.
    train_items = train_items[: len(train_items) // world_size * world_size]
    my_items = train_items[rank::world_size]

    enc_params = [p for n, p in ddp_model.named_parameters() if "encoder." in n]
    head_params = [p for n, p in ddp_model.named_parameters() if "encoder." not in n]
    optimizer = torch.optim.AdamW([{"params": enc_params, "lr": LR_ENCODER}, {"params": head_params, "lr": LR_HEAD}], weight_decay=0.01)
    total_updates = max(1, math.ceil(len(my_items) / (MICRO_BATCH * GRAD_ACCUM)) * EPOCHS)
    scheduler = torch.optim.lr_scheduler.CosineAnnealingLR(optimizer, T_max=total_updates, eta_min=1e-6)
    scaler = torch.amp.GradScaler("cuda", enabled=True)

    if rank == 0:
        print(f"2xT4 DDP: {len(train_items)} train items ({len(calib_items)} held out for calibration), "
              f"{len(my_items)} per rank, {EPOCHS} epochs, effective batch {MICRO_BATCH * world_size * GRAD_ACCUM}", flush=True)
    t0 = time.time()

    for epoch in range(EPOCHS):
        random.seed(42 + epoch + rank)
        random.shuffle(my_items)
        epoch_loss, n_batches, accum_step = 0.0, 0, 0
        optimizer.zero_grad(set_to_none=True)
        sigma = sigma_at(epoch, EPOCHS, SIGMA_START, SIGMA_END)
        steps_per_epoch = math.ceil(len(my_items) / MICRO_BATCH)

        for b_idx in range(0, len(my_items), MICRO_BATCH):
            chunk = my_items[b_idx:b_idx + MICRO_BATCH]
            if not chunk:
                continue
            encoded = [encode_item(tok, it, max_len, head_max_len) for it in chunk]
            batch = collate_items([encoded], tok.pad_token_id)
            with torch.autocast("cuda", dtype=torch.float16):
                logits, _act = ddp_model(batch["input_ids"].to(device), batch["attention_mask"].to(device),
                                         batch["marker_pos"].to(device), batch["marker_mask"].to(device), batch["qtype"].to(device))
            loss = rlcd_loss(logits.float(), batch["target"].to(device), batch["marker_mask"].to(device),
                             batch["qtype"].to(device), sigma, samples=GROUP_SIZE, w_sph=0.75, w_rps=1.0)
            window_start = (accum_step // GRAD_ACCUM) * GRAD_ACCUM
            window_size = min(GRAD_ACCUM, steps_per_epoch - window_start)
            scaler.scale(loss / window_size).backward()
            accum_step += 1
            if accum_step % GRAD_ACCUM == 0 or (b_idx + MICRO_BATCH) >= len(my_items):
                scaler.unscale_(optimizer)
                torch.nn.utils.clip_grad_norm_(ddp_model.parameters(), 1.0)
                scaler.step(optimizer)
                scaler.update()
                scheduler.step()
                optimizer.zero_grad(set_to_none=True)
            epoch_loss += loss.item()
            n_batches += 1
            if rank == 0 and n_batches % 25 == 0:
                print(f"  epoch {epoch + 1}/{EPOCHS} step {n_batches} loss {loss.item():.4f} lr {scheduler.get_last_lr()[0]:.2e}", flush=True)

        if rank == 0:
            print(f"=== epoch {epoch + 1}/{EPOCHS} done in {time.time() - t0:.0f}s, avg loss {epoch_loss / max(1, n_batches):.4f} ===", flush=True)
        dist.barrier()
        if rank == 0:  # rolling checkpoint so a Kaggle timeout does not lose the run
            ckpt_dir = os.path.join(output_dir, "checkpoint_latest")
            save_checkpoint(model, tok, dict(cfg, max_len=max_len, head_max_len=head_max_len), ckpt_dir)
            with open(os.path.join(ckpt_dir, "checkpoint_meta.json"), "w") as f:
                json.dump({"epoch": epoch + 1, "total_epochs": EPOCHS, "avg_loss": epoch_loss / max(1, n_batches)}, f, indent=2)

    dist.barrier()
    if rank == 0:
        print("\nFitting calibration temperatures on the held-out items...", flush=True)
        del optimizer, scaler, scheduler
        torch.cuda.empty_cache()
        model.eval()
        fitted_temps, fitted = None, {"temperature_by_options": {}}
        try:
            records = calibration_records(model, tok, calib_items, device, max_len, head_max_len, batch_size=16)
            fitted = fit_temperature_map(records)
            fitted_temps = fitted["temperature"]
            print("temperatures (choice, score, noul):", [round(t, 3) for t in fitted_temps], flush=True)
        except Exception as e:  # keep the trained weights even if calibration fails
            print("temperature fitting failed, saving uncalibrated:", e, flush=True)
        out_cfg = dict(cfg, max_len=max_len, head_max_len=head_max_len, fine_tuned=True, model_name="laya-claimguard")
        if fitted_temps is not None:
            out_cfg["temperature"] = fitted_temps
            out_cfg.pop("temperature_by_options", None)
            if fitted["temperature_by_options"]:
                out_cfg["temperature_by_options"] = fitted["temperature_by_options"]
        save_checkpoint(model, tok, out_cfg, output_dir)
        print(f"saved to {output_dir}", flush=True)
    dist.destroy_process_group()


if __name__ == "__main__":
    main()
