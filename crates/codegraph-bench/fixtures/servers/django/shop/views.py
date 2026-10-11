from django.http import JsonResponse


def product_list(request):
    return JsonResponse({"items": load_items()})


def load_items():
    return []
