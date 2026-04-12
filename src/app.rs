//! Application library for ems-industrial-gateway.
//!
//! This library provides basic arithmetic functions and configuration management.

/// Adds two 32-bit integers and returns the result.
///
/// # Arguments
/// * `a` - First integer to add
/// * `b` - Second integer to add
///
/// # Returns
/// Sum of the two input integers
///
/// # Example
/// ```
/// use app::add;
/// let result = add(1, 2);
/// assert_eq!(result, 3);
/// ```
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

/// Main application function that adds two integers using the add function.
///
/// # Arguments
/// * `a` - First integer to add
/// * `b` - Second integer to add
///
/// # Returns
/// Sum of the two input integers via the add function
///
/// # Example
/// ```
/// use app::app;
/// let result = app(5, 10);
/// assert_eq!(result, 15);
/// ```
pub fn app(a: i32, b: i32) -> i32 {
    add(a, b)
}
#[cfg(test)]
mod app_test;
