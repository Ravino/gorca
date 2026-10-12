"""Download the small ONNX encoders used by the multimodal benchmarks into .models/ (gitignored)."""
from huggingface_hub import hf_hub_download

FILES = {
    "Xenova/clip-vit-base-patch32": ["onnx/vision_model.onnx", "onnx/vision_model_quantized.onnx",
                                     "onnx/text_model_quantized.onnx", "tokenizer.json", "preprocessor_config.json"],
    "Xenova/siglip-base-patch16-224": ["onnx/vision_model_quantized.onnx", "onnx/text_model_quantized.onnx",
                                       "tokenizer.json", "preprocessor_config.json"],
    "Xenova/mobileclip_s0": ["onnx/vision_model.onnx", "onnx/text_model.onnx", "tokenizer.json",
                             "preprocessor_config.json", "config.json"],
    "Xenova/multilingual-e5-small": ["onnx/model_quantized.onnx", "tokenizer.json"],
    "Xenova/all-MiniLM-L6-v2": ["onnx/model_quantized.onnx", "tokenizer.json"],
}

if __name__ == "__main__":
    for repo, files in FILES.items():
        for f in files:
            p = hf_hub_download(repo, f, local_dir=f".models/{repo.split('/')[1]}")
            print(p)
