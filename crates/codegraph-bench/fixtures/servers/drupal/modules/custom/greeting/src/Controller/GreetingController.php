<?php

namespace Drupal\greeting\Controller;

use Drupal\Core\Controller\ControllerBase;

class GreetingController extends ControllerBase
{
    public function hello(): array
    {
        return ['#markup' => $this->message()];
    }

    private function message(): string
    {
        return 'Hello';
    }
}
