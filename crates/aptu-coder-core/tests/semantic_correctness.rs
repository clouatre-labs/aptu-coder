// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
use aptu_coder_core::analyze;
use tempfile::TempDir;

#[test]
fn test_rust_semantic_correctness() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let rust_file = temp_dir.path().join("main.rs");
    std::fs::write(
        &rust_file,
        r#"use std::collections::HashMap;

pub struct Point {
    x: i32,
    y: i32,
}

impl Point {
    pub fn new(x: i32, y: i32) -> Self { Point { x, y } }
    pub fn distance(&self) -> f64 { 0.0 }
}

pub fn calculate(a: i32, b: i32) -> i32 { a + b }
"#,
    )
    .expect("failed to write rust file");

    let output = analyze::analyze_file(rust_file.to_str().unwrap(), None).expect("analysis failed");

    assert_eq!(
        output.semantic.functions.len(),
        3,
        "Expected 3 functions (new, distance, calculate)"
    );
    assert_eq!(output.semantic.classes.len(), 1, "Expected 1 class (Point)");
    assert_eq!(
        output.semantic.imports.len(),
        1,
        "Expected 1 import (HashMap)"
    );
}

#[test]
fn test_python_semantic_correctness() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let python_file = temp_dir.path().join("main.py");
    std::fs::write(
        &python_file,
        r#"import os
from sys import argv
from collections import defaultdict

def hello():
    pass

def world():
    pass

class MyClass:
    def method(self):
        pass
"#,
    )
    .expect("failed to write python file");

    let output =
        analyze::analyze_file(python_file.to_str().unwrap(), None).expect("analysis failed");

    assert_eq!(
        output.semantic.functions.len(),
        3,
        "Expected 3 functions (hello, world, method)"
    );
    assert_eq!(
        output.semantic.classes.len(),
        1,
        "Expected 1 class (MyClass)"
    );
    assert_eq!(
        output.semantic.imports.len(),
        3,
        "Expected 3 imports (os, sys, collections)"
    );
}

#[test]
fn test_typescript_semantic_correctness() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let ts_file = temp_dir.path().join("main.ts");
    std::fs::write(
        &ts_file,
        r#"import { Component } from 'react';
import * as fs from 'fs';
import path from 'path';

function hello(): void {
    console.log("Hello");
}

interface MyInterface {
    name: string;
}

class MyClass {
    method(): string { return "test"; }
}
"#,
    )
    .expect("failed to write typescript file");

    let output = analyze::analyze_file(ts_file.to_str().unwrap(), None).expect("analysis failed");

    assert!(
        !output.semantic.functions.is_empty(),
        "Expected at least 1 function"
    );
    assert!(
        output.semantic.classes.len() >= 2,
        "Expected at least 2 classes (MyInterface, MyClass)"
    );
    assert_eq!(
        output.semantic.imports.len(),
        3,
        "Expected 3 imports (react, fs, path)"
    );
}

#[test]
fn test_go_semantic_correctness() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let go_file = temp_dir.path().join("main.go");
    std::fs::write(
        &go_file,
        r#"package main

import (
    "fmt"
    "os"
)

import "io"

func Hello() {
    fmt.Println("Hello")
}

func main() {
    fmt.Println("main")
}

type MyStruct struct {
    Name string
}

type MyInterface interface {
    Method()
}
"#,
    )
    .expect("failed to write go file");

    let output = analyze::analyze_file(go_file.to_str().unwrap(), None).expect("analysis failed");

    assert!(
        !output.semantic.functions.is_empty(),
        "Expected at least 1 function"
    );
    assert!(
        output.semantic.classes.len() >= 2,
        "Expected at least 2 classes (MyStruct, MyInterface)"
    );
    assert_eq!(
        output.semantic.imports.len(),
        2,
        "Expected 2 import blocks (one multi-line import group, one single import)"
    );
}

#[test]
fn test_java_semantic_correctness() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let java_file = temp_dir.path().join("Test.java");
    std::fs::write(
        &java_file,
        r#"import java.util.ArrayList;
import java.util.List;
import static java.lang.Math.sqrt;

public class Test {
    public void method() {
        System.out.println("Hello");
    }
}
"#,
    )
    .expect("failed to write java file");

    let output = analyze::analyze_file(java_file.to_str().unwrap(), None).expect("analysis failed");

    assert!(
        !output.semantic.functions.is_empty(),
        "Expected at least 1 function (method)"
    );
    assert!(
        !output.semantic.classes.is_empty(),
        "Expected at least 1 class (Test)"
    );
    assert_eq!(
        output.semantic.imports.len(),
        3,
        "Expected 3 imports (ArrayList, List, Math)"
    );
}

#[test]
fn test_csharp_semantic_correctness() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let cs_file = temp_dir.path().join("Sample.cs");
    std::fs::write(
        &cs_file,
        r#"using System;
using System.Collections.Generic;

namespace Demo {
    public interface IAnimal {
        void Speak();
    }

    public class Animal : IAnimal {
        public void Speak() {
            Console.WriteLine("...");
        }
    }

    public class Dog : Animal {
        public Dog() {}
        public override void Speak() {
            Console.WriteLine("Woof");
        }
        public async Task FetchAsync() {
            await Task.Delay(1);
        }
    }
}
"#,
    )
    .expect("failed to write C# file");

    let output = analyze::analyze_file(cs_file.to_str().unwrap(), None).expect("analysis failed");

    assert!(
        output.semantic.functions.len() >= 3,
        "Expected at least 3 functions (constructor + Speak overrides + FetchAsync), got {}",
        output.semantic.functions.len()
    );
    assert!(
        output.semantic.classes.len() >= 3,
        "Expected at least 3 types (IAnimal, Animal, Dog), got {}",
        output.semantic.classes.len()
    );
    assert_eq!(
        output.semantic.imports.len(),
        2,
        "Expected 2 using directives"
    );

    // Dog should carry its base type
    let dog = output
        .semantic
        .classes
        .iter()
        .find(|c| c.name == "Dog")
        .expect("Dog class not found");
    assert!(
        dog.inherits.iter().any(|i| i.contains("Animal")),
        "Dog should inherit from Animal"
    );
}
